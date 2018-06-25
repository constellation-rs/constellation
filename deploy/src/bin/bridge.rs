/*

TODO: can lose processes such that ctrl+c doesn't kill them. i think if we kill while spawning.

*/

#![feature(nll)]
extern crate bincode;
extern crate crossbeam;
extern crate deploy;
extern crate deploy_common;
extern crate either;
extern crate nix;

use either::Either;
use std::{
	collections::HashMap,
	env, ffi, fs,
	io::{self, Read, Write},
	mem, net,
	os::{
		self,
		unix::{
			self,
			io::{AsRawFd, FromRawFd, IntoRawFd},
		},
	},
	path::{self, PathBuf},
	sync::{self, mpsc},
	thread, *,
};

use deploy_common::{
	copy, copy_sendfile, map_bincode_err, memfd_create, move_fds, seal, BufferedStream, DeployInputEvent, DeployOutputEvent, FdIter, Pid, ProcessInputEvent, ProcessOutputEvent, Resources, Signal
};

macro_rules! log { // prints to STDOUT
	($($arg:tt)*) => (let mut file = ::std::io::BufWriter::with_capacity(4096/*PIPE_BUF*/, unsafe{<::std::fs::File as ::std::os::unix::io::FromRawFd>::from_raw_fd(2)}); <::std::io::BufWriter<::std::fs::File> as ::std::io::Write>::write_fmt(&mut file, format_args!($($arg)*)).unwrap(); <::std::fs::File as ::std::os::unix::io::IntoRawFd>::into_raw_fd(file.into_inner().unwrap()););
}
// macro_rules! log {
// 	($($arg:tt)*) => ();
// }
macro_rules! logln {
	() => (log!("\n"));
	($fmt:expr) => (log!(concat!($fmt, "\n")));
	($fmt:expr, $($arg:tt)*) => (log!(concat!($fmt, "\n"), $($arg)*));
}
macro_rules! print {
	($($arg:tt)*) => {
		compile_error!("Cannot use print!()")
	};
}
macro_rules! eprint {
	($($arg:tt)*) => {
		compile_error!("Cannot use eprint!()")
	};
}

const LISTENER_FD: os::unix::io::RawFd = 3; // from fabric
const ARG_FD: os::unix::io::RawFd = 4; // from fabric
const BOUND_FD: os::unix::io::RawFd = 5; // from fabric
const SCHEDULER_FD: os::unix::io::RawFd = 4;

#[derive(Clone, Debug)]
enum OutputEventInt {
	Spawn(Pid, Pid, mpsc::SyncSender<InputEventInt>),
	Output(Pid, os::unix::io::RawFd, Vec<u8>),
	Exit(Pid, Either<u8, Signal>),
}
#[derive(Clone, Debug)]
enum InputEventInt {
	Input(os::unix::io::RawFd, Vec<u8>),
	Kill,
}

fn parse_request<R: Read>(
	mut stream: &mut R,
) -> Result<
	(
		Option<Resources>,
		Vec<ffi::OsString>,
		Vec<(ffi::OsString, ffi::OsString)>,
		fs::File,
		Vec<u8>,
	),
	io::Error,
> {
	let process = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let args: Vec<ffi::OsString> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let vars: Vec<(ffi::OsString, ffi::OsString)> =
		bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let len: u64 = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	// let mut elf = Vec::with_capacity(len as usize);
	// copy(stream, &mut elf, len as usize)?; assert_eq!(elf.len(), len as usize);
	let mut elf = unsafe {
		fs::File::from_raw_fd(
			memfd_create(
				&ffi::CString::new(unix::ffi::OsStringExt::into_vec(args[0].clone())).unwrap(),
				nix::sys::memfd::MemFdCreateFlag::MFD_CLOEXEC,
			).expect("Failed to memfd_create"),
		)
	};
	assert!(
		nix::fcntl::FdFlag::from_bits(
			nix::fcntl::fcntl(elf.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFD).unwrap()
		).unwrap()
			.contains(nix::fcntl::FdFlag::FD_CLOEXEC)
	);
	nix::unistd::ftruncate(elf.as_raw_fd(), len as i64).unwrap();
	copy(stream, &mut elf, len as usize)?;
	let x = nix::unistd::lseek64(elf.as_raw_fd(), 0, nix::unistd::Whence::SeekSet).unwrap();
	assert_eq!(x, 0);
	seal(elf.as_raw_fd());

	let arg: Vec<u8> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	Ok((process, args, vars, elf, arg))
}

static PROCESS_COUNT: sync::atomic::AtomicUsize = sync::atomic::AtomicUsize::new(0);

fn monitor_process(
	pid: Pid, sender_: mpsc::SyncSender<OutputEventInt>, receiver_: mpsc::Receiver<InputEventInt>,
) {
	let receiver = deploy::Receiver::new(pid);
	let sender = deploy::Sender::new(pid);
	thread::spawn(move || {
		for event in receiver_.iter() {
			let event = match event {
				InputEventInt::Input(fd, input) => ProcessInputEvent::Input(fd, input),
				InputEventInt::Kill => ProcessInputEvent::Kill,
			};
			match sender.send(event) {
				Ok(()) => (),
				Err(deploy::ChannelError::Exited) => break, // TODO,
				Err(e) => panic!("BRIDGE send fail: {:?}", e),
			}
			// if let Err(_) = bincode::serialize_into(&mut sender, &event) {
			// 	break; // TODO: remove
			// }
		}
		for _event in receiver_ {}
		let x = PROCESS_COUNT.fetch_sub(1, sync::atomic::Ordering::Relaxed);
		assert_ne!(x, 0);
		logln!("BRIDGE: KILL ({})", x);
	});
	loop {
		// let event: Result<ProcessOutputEvent,_> = bincode::deserialize_from(&mut receiver).map_err(map_bincode_err);
		let event: ProcessOutputEvent = receiver.recv().expect("BRIDGE recv fail");
		// if event.is_err() {
		// 	logln!("BRIDGE: {:?} died {:?}", pid, event.err().unwrap());
		// 	sender_.send(OutputEventInt::Exit(pid, 101)).unwrap();
		// 	break;
		// }
		match event {
			//.unwrap() {
			ProcessOutputEvent::Spawn(new_pid) => {
				let x = PROCESS_COUNT.fetch_add(1, sync::atomic::Ordering::Relaxed);
				logln!("BRIDGE: SPAWN ({})", x);
				let (sender1, receiver1) = mpsc::sync_channel::<_>(0);
				sender_
					.send(OutputEventInt::Spawn(pid, new_pid, sender1))
					.unwrap();
				let sender_ = sender_.clone();
				thread::Builder::new()
					.name(String::from("d"))
					.spawn(move || {
						monitor_process(new_pid, sender_, receiver1);
					})
					.unwrap();
			}
			ProcessOutputEvent::Output(fd, output) => {
				sender_
					.send(OutputEventInt::Output(pid, fd, output))
					.unwrap();
			}
			ProcessOutputEvent::Exit(exit_code) => {
				sender_.send(OutputEventInt::Exit(pid, exit_code)).unwrap();
				break;
			}
		}
	}
}

fn recce(
	elf: &fs::File, args: &Vec<ffi::OsString>, vars: &Vec<(ffi::OsString, ffi::OsString)>,
) -> Result<Resources, ()> {
	let (reader, writer) = nix::unistd::pipe2(nix::fcntl::OFlag::empty()).unwrap();
	let child = if let nix::unistd::ForkResult::Parent { child, .. } =
		nix::unistd::fork().expect("Fork failed")
	{
		child
	} else {
		// println!("{:?}", args[0]);
		let err = unsafe { nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL) };
		assert_eq!(err, 0);
		let vars = iter::once((
			ffi::CString::new("DEPLOY").unwrap(),
			ffi::CString::new("fabric").unwrap(),
		)).chain(iter::once((
			ffi::CString::new("DEPLOY_RECCE").unwrap(),
			ffi::CString::new("1").unwrap(),
		)))
			.chain(vars.into_iter().map(|(x, y)| {
				(
					ffi::CString::new(unix::ffi::OsStringExt::into_vec(x.clone())).unwrap(),
					ffi::CString::new(unix::ffi::OsStringExt::into_vec(y.clone())).unwrap(),
				)
			}))
			.map(|(key, value)| {
				ffi::CString::new(format!(
					"{}={}",
					key.to_str().unwrap(),
					value.to_str().unwrap()
				)).unwrap()
			})
			.collect::<Vec<_>>();
		nix::unistd::close(reader).unwrap();
		for fd in FdIter::new().filter(|&fd| fd != elf.as_raw_fd() && fd != writer) {
			nix::unistd::close(fd).unwrap();
		}
		move_fds(&mut [(writer, 3), (elf.as_raw_fd(), 4)]);
		let err = nix::fcntl::open(
			"/dev/null",
			nix::fcntl::OFlag::O_RDWR,
			nix::sys::stat::Mode::empty(),
		).unwrap();
		assert_eq!(err, nix::libc::STDIN_FILENO);
		let err = nix::unistd::dup(nix::libc::STDIN_FILENO).unwrap();
		assert_eq!(err, nix::libc::STDOUT_FILENO);
		let err = nix::unistd::dup(nix::libc::STDIN_FILENO).unwrap();
		assert_eq!(err, nix::libc::STDERR_FILENO);
		if false {
			nix::unistd::execve(
				&ffi::CString::new(unix::ffi::OsStringExt::into_vec(args[0].clone())).unwrap(),
				&args
					.into_iter()
					.map(|x| {
						ffi::CString::new(unix::ffi::OsStringExt::into_vec(x.clone())).unwrap()
					})
					.collect::<Vec<_>>(),
				&vars,
			).expect("Failed to fexecve ELF");
		} else {
			// 			if is_valgrind() {
			// 				let elf_desired_fd_ = valgrind_start_fd()-1; assert!(elf_desired_fd_ > elf_desired_fd);
			// 				nix::unistd::dup2(elf_desired_fd, elf_desired_fd_).unwrap();
			// 				nix::unistd::close(elf_desired_fd).unwrap();
			// 				elf_desired_fd = elf_desired_fd_;
			// 			}
			nix::unistd::fexecve(
				4,
				&args
					.into_iter()
					.map(|x| {
						ffi::CString::new(unix::ffi::OsStringExt::into_vec(x.clone())).unwrap()
					})
					.collect::<Vec<_>>(),
				&vars,
			).expect("Failed to fexecve ELF");
		}
		unreachable!();
	};
	nix::unistd::close(writer).unwrap();
	thread::Builder::new()
		.spawn(move || {
			thread::sleep(time::Duration::new(1, 0));
			let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
		})
		.unwrap();
	match nix::sys::wait::waitpid(child, None).unwrap() {
		nix::sys::wait::WaitStatus::Exited(pid, code) if code == 0 => assert_eq!(pid, child),
		nix::sys::wait::WaitStatus::Signaled(pid, signal, _)
			if signal == nix::sys::signal::Signal::SIGKILL =>
		{
			assert_eq!(pid, child)
		}
		wait_status => panic!("{:?}", wait_status),
	}
	let reader = unsafe { fs::File::from_raw_fd(reader) };
	bincode::deserialize_from(&mut &reader)
		.map_err(map_bincode_err)
		.map_err(|_| ())
}

fn main() {
	env::set_var("RUST_BACKTRACE", "full");
	logln!("BRIDGE: Resources: {:?}", ()); // TODO
	let listener = deploy::bridge_init();
	let (sender, receiver) = mpsc::sync_channel::<_>(0);
	thread::Builder::new()
		.name(String::from("a"))
		.spawn(move || {
			for stream in listener.incoming() {
				logln!("BRIDGE: accepted");
				let mut stream = stream.unwrap();
				let sender = sender.clone();
				thread::Builder::new()
					.name(String::from("b"))
					.spawn(move || {
						let stream = stream;
						let (mut stream_read, mut stream_write) =
							(BufferedStream::new(&stream), &stream);
						if let Ok((process, args, vars, elf, mut arg)) =
							parse_request(&mut stream_read)
						{
							assert_eq!(arg.len(), 0);
							bincode::serialize_into(&mut arg, &deploy::pid()).unwrap();
							let (sender_, receiver) = mpsc::sync_channel::<_>(0);
							sender
								.send((
									process.unwrap_or_else(|| recce(&elf, &args, &vars).unwrap()),
									args,
									vars,
									elf,
									arg,
									sender_,
								))
								.unwrap();
							let pid: Option<Pid> = receiver.recv().unwrap();
							bincode::serialize_into(&mut stream_write, &pid).unwrap(); // TODO: catch this failing
							if let Some(pid) = pid {
								let x = PROCESS_COUNT.fetch_add(1, sync::atomic::Ordering::Relaxed);
								logln!("BRIDGE: SPAWN ({})", x);
								let (sender, receiver) = mpsc::sync_channel::<_>(0);
								let (sender1, receiver1) = mpsc::sync_channel::<_>(0);
								thread::Builder::new()
									.name(String::from("c"))
									.spawn(move || {
										monitor_process(pid, sender, receiver1);
									})
									.unwrap();
								let hashmap = &sync::Mutex::new(HashMap::new());
								hashmap.lock().unwrap().insert(pid, sender1);
								crossbeam::scope(|scope| {
									scope.spawn(move || {
										loop {
											let event: Result<DeployInputEvent,_> = bincode::deserialize_from(&mut stream_read).map_err(map_bincode_err);
											if let Err(_) = event {
												break;
											}
											match event.unwrap() {
												DeployInputEvent::Input(pid, fd, input) => {
													hashmap
														.lock()
														.unwrap()
														.get(&pid)
														.unwrap()
														.send(InputEventInt::Input(fd, input))
														.unwrap();
												}
												DeployInputEvent::Kill(Some(pid)) => {
													hashmap
														.lock()
														.unwrap()
														.get(&pid)
														.unwrap()
														.send(InputEventInt::Kill)
														.unwrap();
												}
												DeployInputEvent::Kill(None) => {
													break;
												}
											}
										}
										let x = hashmap.lock().unwrap();
										for (_, process) in x.iter() {
											process.send(InputEventInt::Kill).unwrap();
										}
									});
									for event in receiver.iter() {
										let event = match event {
											OutputEventInt::Spawn(pid, new_pid, sender) => {
												let x =
													hashmap.lock().unwrap().insert(new_pid, sender);
												assert!(x.is_none());
												DeployOutputEvent::Spawn(pid, new_pid)
											}
											OutputEventInt::Output(pid, fd, output) => {
												DeployOutputEvent::Output(pid, fd, output)
											}
											OutputEventInt::Exit(pid, exit_code) => {
												hashmap.lock().unwrap().remove(&pid).unwrap();
												DeployOutputEvent::Exit(pid, exit_code)
											}
										};
										if let Err(_) =
											bincode::serialize_into(&mut stream_write, &event)
										{
											break;
										}
									}
									logln!("BRIDGE: KILLED: {:?}", *hashmap.lock().unwrap());
									let mut x = hashmap.lock().unwrap();
									for (_, process) in x.drain() {
										process.send(InputEventInt::Kill).unwrap();
									}
									for _event in receiver {}
								});
								assert_eq!(
									hashmap.lock().unwrap().len(),
									0,
									"{:?}",
									*hashmap.lock().unwrap()
								);
							}
						}
					})
					.unwrap();
			}
		})
		.unwrap();

	for (process, args, vars, elf, arg, sender) in receiver {
		let scheduler = unsafe { fs::File::from_raw_fd(SCHEDULER_FD) };
		let (mut scheduler_read, mut scheduler_write) = (
			BufferedStream::new(&scheduler),
			BufferedStream::new(&scheduler),
		);

		let len: u64 = elf.metadata().unwrap().len();
		assert_ne!(len, 0);
		let mut scheduler_write_ = scheduler_write.write();
		bincode::serialize_into(&mut scheduler_write_, &process).unwrap();
		bincode::serialize_into(&mut scheduler_write_, &args).unwrap();
		bincode::serialize_into(&mut scheduler_write_, &vars).unwrap();
		bincode::serialize_into(&mut scheduler_write_, &len).unwrap();
		mem::drop(scheduler_write_);
		copy_sendfile(&**scheduler_write.get_ref(), &elf, len as usize).unwrap();
		let mut scheduler_write_ = scheduler_write.write();
		bincode::serialize_into(&mut scheduler_write_, &arg).unwrap();
		mem::drop(scheduler_write_);

		let pid: Option<Pid> = bincode::deserialize_from(&mut scheduler_read)
			.map_err(map_bincode_err)
			.unwrap();
		sender.send(pid).unwrap();
		mem::drop((scheduler_read, scheduler_write));
		scheduler.into_raw_fd();
	}
}
