//! A binary that runs automatically on the `fabric` that is responsible for forwarding output back to `cargo deploy` at the user's terminal.

/*

TODO: can lose processes such that ctrl+c doesn't kill them. i think if we kill while spawning.

*/

#![warn(
	missing_copy_implementations,
	missing_debug_implementations,
	missing_docs,
	trivial_numeric_casts,
	unused_extern_crates,
	unused_import_braces,
	unused_qualifications,
	unused_results,
	clippy::pedantic
)] // from https://github.com/rust-unofficial/patterns/blob/master/anti_patterns/deny-warnings.md
#![allow(
	clippy::similar_names,
	clippy::type_complexity,
	clippy::shadow_unrelated
)]

use log::trace;
use palaver::file::{copy, copy_sendfile, fexecve, memfd_create, move_fds, seal_fd, FdIter};
use std::{
	collections::HashMap, convert::TryInto, env, ffi::{CString, OsString}, fs, io::{self, Read}, iter, os::{
		self, unix::{
			ffi::OsStringExt, io::{AsRawFd, FromRawFd, IntoRawFd}
		}
	}, sync::{self, mpsc}, thread, time
};

use constellation_internal::{
	map_bincode_err, BufferedStream, DeployInputEvent, DeployOutputEvent, ExitStatus, Pid, ProcessInputEvent, ProcessOutputEvent, Resources
};

#[cfg(target_family = "unix")]
type Fd = os::unix::io::RawFd;
#[cfg(target_family = "windows")]
type Fd = os::windows::io::RawHandle;

const SCHEDULER_FD: Fd = 4;

#[derive(Clone, Debug)]
enum OutputEventInt {
	Spawn(Pid, Pid, mpsc::SyncSender<InputEventInt>),
	Output(Pid, Fd, Vec<u8>),
	Exit(Pid, ExitStatus),
}
#[derive(Clone, Debug)]
enum InputEventInt {
	Input(Fd, Vec<u8>),
	Kill,
}

fn parse_request<R: Read>(
	mut stream: &mut R,
) -> Result<
	(
		Option<Resources>,
		Vec<OsString>,
		Vec<(OsString, OsString)>,
		fs::File,
		Vec<u8>,
	),
	io::Error,
> {
	let process = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let args: Vec<OsString> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let vars: Vec<(OsString, OsString)> =
		bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let len: u64 = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	// let mut binary = Vec::with_capacity(len as usize);
	// copy(stream, &mut binary, len as usize)?; assert_eq!(binary.len(), len as usize);
	let mut binary = unsafe {
		fs::File::from_raw_fd(
			memfd_create(
				&CString::new(OsStringExt::into_vec(args[0].clone())).unwrap(),
				true,
			)
			.expect("Failed to memfd_create"),
		)
	};
	assert!(nix::fcntl::FdFlag::from_bits(
		nix::fcntl::fcntl(binary.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFD).unwrap()
	)
	.unwrap()
	.contains(nix::fcntl::FdFlag::FD_CLOEXEC));
	nix::unistd::ftruncate(binary.as_raw_fd(), len.try_into().unwrap()).unwrap();
	copy(stream, &mut binary, len)?;
	let x = nix::unistd::lseek(binary.as_raw_fd(), 0, nix::unistd::Whence::SeekSet).unwrap();
	assert_eq!(x, 0);
	seal_fd(binary.as_raw_fd());

	let arg: Vec<u8> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	Ok((process, args, vars, binary, arg))
}

static PROCESS_COUNT: sync::atomic::AtomicUsize = sync::atomic::AtomicUsize::new(0);

fn monitor_process(
	pid: Pid, sender_: mpsc::SyncSender<OutputEventInt>, receiver_: mpsc::Receiver<InputEventInt>,
) {
	let receiver = constellation::Receiver::new(pid);
	let sender = constellation::Sender::new(pid);
	let _ = thread::Builder::new()
		.name(String::from("monitor_process"))
		.spawn(move || {
			for event in receiver_.iter() {
				let event = match event {
					InputEventInt::Input(fd, input) => ProcessInputEvent::Input(fd, input),
					InputEventInt::Kill => ProcessInputEvent::Kill,
				};
				sender.send(event);
				//  {
				// 	Ok(()) => (),
				// 	Err(constellation::ChannelError::Exited) => break, // TODO,
				// 	Err(e) => panic!("BRIDGE send fail: {:?}", e),
				// }
				// if let Err(_) = bincode::serialize_into(&mut sender, &event) {
				// 	break; // TODO: remove
				// }
			}
			for _event in receiver_ {}
			let x = PROCESS_COUNT.fetch_sub(1, sync::atomic::Ordering::Relaxed);
			assert_ne!(x, 0);
			trace!("BRIDGE: KILL ({})", x);
		})
		.unwrap();
	loop {
		// let event: Result<ProcessOutputEvent,_> = bincode::deserialize_from(&mut receiver).map_err(map_bincode_err);
		let event: ProcessOutputEvent = receiver.recv().expect("BRIDGE recv fail");
		// if event.is_err() {
		// 	trace!("BRIDGE: {:?} died {:?}", pid, event.err().unwrap());
		// 	sender_.send(OutputEventInt::Exit(pid, 101)).unwrap();
		// 	break;
		// }
		match event {
			//.unwrap() {
			ProcessOutputEvent::Spawn(new_pid) => {
				let x = PROCESS_COUNT.fetch_add(1, sync::atomic::Ordering::Relaxed);
				trace!("BRIDGE: SPAWN ({})", x);
				let (sender1, receiver1) = mpsc::sync_channel::<_>(0);
				sender_
					.send(OutputEventInt::Spawn(pid, new_pid, sender1))
					.unwrap();
				let sender_ = sender_.clone();
				let _ = thread::Builder::new()
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
	drop(sender_); // placate clippy needless_pass_by_value
}

fn recce(
	binary: &fs::File, args: &[OsString], vars: &[(OsString, OsString)],
) -> Result<Resources, ()> {
	let (reader, writer) = nix::unistd::pipe().unwrap();

	let vars = iter::once((
		CString::new("CONSTELLATION").unwrap(),
		CString::new("fabric").unwrap(),
	))
	.chain(iter::once((
		CString::new("CONSTELLATION_RECCE").unwrap(),
		CString::new("1").unwrap(),
	)))
	.chain(vars.iter().map(|(x, y)| {
		(
			CString::new(OsStringExt::into_vec(x.clone())).unwrap(),
			CString::new(OsStringExt::into_vec(y.clone())).unwrap(),
		)
	}))
	.map(|(key, value)| {
		CString::new(format!(
			"{}={}",
			key.to_str().unwrap(),
			value.to_str().unwrap()
		))
		.unwrap()
	})
	.collect::<Vec<_>>();

	let args = args
		.iter()
		.map(|x| CString::new(OsStringExt::into_vec(x.clone())).unwrap())
		.collect::<Vec<_>>();

	let child = if let nix::unistd::ForkResult::Parent { child, .. } =
		nix::unistd::fork().expect("Fork failed")
	{
		child
	} else {
		// Memory can be in a weird state now. Imagine a thread has just taken out a lock,
		// but we've just forked. Lock still held. Avoid deadlock by doing nothing fancy here.
		// Ideally including malloc.

		// println!("{:?}", args[0]);
		#[cfg(any(target_os = "android", target_os = "linux"))]
		{
			let err = unsafe { nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL) };
			assert_eq!(err, 0);
		}
		nix::unistd::close(reader).unwrap();
		for fd in FdIter::new()
			.unwrap()
			.filter(|&fd| fd != binary.as_raw_fd() && fd != writer)
		{
			nix::unistd::close(fd).unwrap();
		}
		move_fds(
			&mut [(writer, 3), (binary.as_raw_fd(), 4)],
			Some(nix::fcntl::FdFlag::empty()),
			true,
		);
		let err = nix::fcntl::open(
			"/dev/null",
			nix::fcntl::OFlag::O_RDWR,
			nix::sys::stat::Mode::empty(),
		)
		.unwrap();
		assert_eq!(err, nix::libc::STDIN_FILENO);
		let err = nix::unistd::dup(nix::libc::STDIN_FILENO).unwrap();
		assert_eq!(err, nix::libc::STDOUT_FILENO);
		let err = nix::unistd::dup(nix::libc::STDIN_FILENO).unwrap();
		assert_eq!(err, nix::libc::STDERR_FILENO);
		if false {
			nix::unistd::execve(&args[0], &args, &vars).expect("Failed to fexecve ELF");
		} else {
			// if is_valgrind() {
			// 	let binary_desired_fd_ = valgrind_start_fd()-1; assert!(binary_desired_fd_ > binary_desired_fd);
			// 	nix::unistd::dup2(binary_desired_fd, binary_desired_fd_).unwrap();
			// 	nix::unistd::close(binary_desired_fd).unwrap();
			// 	binary_desired_fd = binary_desired_fd_;
			// }
			fexecve(4, &args, &vars).expect("Failed to fexecve ELF");
		}
		unreachable!();
	};
	nix::unistd::close(writer).unwrap();
	let _ = thread::Builder::new()
		.name(String::from(""))
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
	trace!("BRIDGE: Resources: {:?}", ()); // TODO
	let listener = constellation::bridge_init();
	let (sender, receiver) = mpsc::sync_channel::<_>(0);
	let _ = thread::Builder::new()
		.name(String::from("a"))
		.spawn(move || {
			for stream in listener.incoming() {
				trace!("BRIDGE: accepted");
				let stream = stream.unwrap();
				let sender = sender.clone();
				let _ = thread::Builder::new()
					.name(String::from("b"))
					.spawn(move || {
						let stream = stream;
						#[cfg(not(any(target_os = "macos", target_os = "ios")))]
						nix::sys::socket::setsockopt(
							stream.as_raw_fd(),
							nix::sys::socket::sockopt::Linger,
							&nix::libc::linger {
								l_onoff: 1,
								l_linger: 10,
							},
						)
						.unwrap();
						#[cfg(any(target_os = "macos", target_os = "ios"))]
						{
							const SO_LINGER_SEC: nix::libc::c_int = 0x1080;
							let err = unsafe {
								nix::libc::setsockopt(
									stream.as_raw_fd(),
									nix::libc::SOL_SOCKET,
									SO_LINGER_SEC,
									&nix::libc::linger {
										l_onoff: 1,
										l_linger: 10,
									} as *const nix::libc::linger as *const nix::libc::c_void,
									std::mem::size_of::<nix::libc::linger>().try_into().unwrap(),
								)
							};
							assert_eq!(err, 0);
						}
						let (mut stream_read, mut stream_write) =
							(BufferedStream::new(&stream), &stream);
						if let Ok((process, args, vars, binary, mut arg)) =
							parse_request(&mut stream_read)
						{
							assert_eq!(arg.len(), 0);
							bincode::serialize_into(&mut arg, &constellation::pid()).unwrap();
							let (sender_, receiver) = mpsc::sync_channel::<_>(0);
							sender
								.send((
									process
										.unwrap_or_else(|| recce(&binary, &args, &vars).unwrap()),
									args,
									vars,
									binary,
									arg,
									sender_,
								))
								.unwrap();
							let pid: Option<Pid> = receiver.recv().unwrap();
							bincode::serialize_into(&mut stream_write, &pid).unwrap(); // TODO: catch this failing
							if let Some(pid) = pid {
								let x = PROCESS_COUNT.fetch_add(1, sync::atomic::Ordering::Relaxed);
								trace!("BRIDGE: SPAWN ({})", x);
								let (sender, receiver) = mpsc::sync_channel::<_>(0);
								let (sender1, receiver1) = mpsc::sync_channel::<_>(0);
								let _ = thread::Builder::new().name(String::from("c")).spawn(
									move || {
										monitor_process(pid, sender, receiver1);
									},
								);
								let hashmap = &sync::Mutex::new(HashMap::new());
								let _ = hashmap.lock().unwrap().insert(pid, sender1);
								crossbeam::scope(|scope| {
									let _ = scope.spawn(move |_scope| {
										loop {
											let event: Result<DeployInputEvent, _> =
												bincode::deserialize_from(&mut stream_read)
													.map_err(map_bincode_err);
											if event.is_err() {
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
												let _ =
													hashmap.lock().unwrap().remove(&pid).unwrap();
												DeployOutputEvent::Exit(pid, exit_code)
											}
										};
										if bincode::serialize_into(&mut stream_write, &event)
											.is_err()
										{
											break;
										}
									}
									trace!("BRIDGE: KILLED: {:?}", *hashmap.lock().unwrap());
									let mut x = hashmap.lock().unwrap();
									for (_, process) in x.drain() {
										process.send(InputEventInt::Kill).unwrap();
									}
									for _event in receiver {}
								})
								.unwrap();
								assert_eq!(
									hashmap.lock().unwrap().len(),
									0,
									"{:?}",
									*hashmap.lock().unwrap()
								);
							}
							nix::sys::socket::shutdown(
								stream.as_raw_fd(),
								nix::sys::socket::Shutdown::Write,
							)
							.unwrap();
						}
					})
					.unwrap();
			}
		})
		.unwrap();

	for (process, args, vars, binary, arg, sender) in receiver {
		let scheduler = unsafe { fs::File::from_raw_fd(SCHEDULER_FD) };
		let (mut scheduler_read, mut scheduler_write) = (
			BufferedStream::new(&scheduler),
			BufferedStream::new(&scheduler),
		);

		let len: u64 = binary.metadata().unwrap().len();
		assert_ne!(len, 0);
		let mut scheduler_write_ = scheduler_write.write();
		bincode::serialize_into(&mut scheduler_write_, &process).unwrap();
		bincode::serialize_into(&mut scheduler_write_, &args).unwrap();
		bincode::serialize_into(&mut scheduler_write_, &vars).unwrap();
		bincode::serialize_into(&mut scheduler_write_, &len).unwrap();
		drop(scheduler_write_);
		copy_sendfile(&binary, &**scheduler_write.get_ref(), len).unwrap();
		let mut scheduler_write_ = scheduler_write.write();
		bincode::serialize_into(&mut scheduler_write_, &arg).unwrap();
		drop(scheduler_write_);

		let pid: Option<Pid> = bincode::deserialize_from(&mut scheduler_read)
			.map_err(map_bincode_err)
			.unwrap();
		sender.send(pid).unwrap();
		drop((scheduler_read, scheduler_write));
		let _ = scheduler.into_raw_fd();
	}
}
