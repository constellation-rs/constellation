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

use either::Either;
use futures::{sink::SinkExt, stream::StreamExt};
use log::trace;
use palaver::file::{fexecve, move_fds, seal_fd};
use std::{
	collections::HashMap, ffi::{CString, OsString}, fs::File, io::{self, Read}, iter, net::TcpStream, os::unix::{
		ffi::OsStringExt, io::{AsRawFd, FromRawFd, IntoRawFd}
	}, sync::{
		atomic::{self, AtomicUsize}, mpsc, Mutex
	}, thread, time::Duration
};

use constellation_internal::{
	file_from_reader, forbid_alloc, map_bincode_err, msg::{bincode_serialize_into, FabricRequest}, BufferedStream, DeployInputEvent, DeployOutputEvent, ExitStatus, Fd, Pid, ProcessInputEvent, ProcessOutputEvent, Resources
};

const SCHEDULER_FD: Fd = 4;
const _RECCE_TIMEOUT: Duration = Duration::from_secs(10); // Time to allow binary to call init()

#[derive(Clone, Debug)]
enum OutputEventInt {
	Spawn(Pid, Pid, futures::channel::mpsc::Sender<InputEventInt>),
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
		File,
		Vec<u8>,
	),
	io::Error,
> {
	let process = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let args: Vec<OsString> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let vars: Vec<(OsString, OsString)> =
		bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let arg: Vec<u8> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let binary_len: u64 = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let binary = file_from_reader(&mut stream, binary_len, &args[0], true)?;
	seal_fd(binary.as_raw_fd());
	Ok((process, args, vars, binary, arg))
}

static PROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);

fn monitor_process(
	pid: Pid, sender_: mpsc::SyncSender<Either<OutputEventInt, Option<DeployInputEvent>>>,
	mut receiver_: futures::channel::mpsc::Receiver<InputEventInt>,
) {
	let receiver = constellation::Receiver::new(pid);
	let sender = constellation::Sender::new(pid);
	loop {
		let mut event = None;
		let x = match futures::executor::block_on(futures::future::select(
			receiver_.next(),
			receiver.selectable_recv(|t| event = Some(t)),
		)) {
			futures::future::Either::Left((a, _)) => futures::future::Either::Left(a),
			futures::future::Either::Right(((), _)) => futures::future::Either::Right(()),
		};
		match x {
			futures::future::Either::Left(event) => {
				sender.send(match event.unwrap() {
					InputEventInt::Input(fd, input) => ProcessInputEvent::Input(fd, input),
					InputEventInt::Kill => ProcessInputEvent::Kill,
				});
			}
			futures::future::Either::Right(()) => {
				let event = event.unwrap();
				match event.unwrap() {
					ProcessOutputEvent::Spawn(new_pid) => {
						let (sender1, receiver1) = futures::channel::mpsc::channel(0);
						sender_
							.send(Either::Left(OutputEventInt::Spawn(pid, new_pid, sender1)))
							.unwrap();
						let sender_ = sender_.clone();
						let _ = thread::Builder::new()
							.name(String::from("d"))
							.spawn(move || monitor_process(new_pid, sender_, receiver1))
							.unwrap();
					}
					ProcessOutputEvent::Output(fd, output) => {
						sender_
							.send(Either::Left(OutputEventInt::Output(pid, fd, output)))
							.unwrap();
					}
					ProcessOutputEvent::Exit(exit_code) => {
						sender_
							.send(Either::Left(OutputEventInt::Exit(pid, exit_code)))
							.unwrap();
						break;
					}
				}
			}
		}
	}
	drop(sender_); // placate clippy needless_pass_by_value
}

fn recce(binary: &File, args: &[OsString], vars: &[(OsString, OsString)]) -> Result<Resources, ()> {
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

	let args_p = Vec::with_capacity(args.len() + 1);
	let vars_p = Vec::with_capacity(vars.len() + 1);

	let child = if let nix::unistd::ForkResult::Parent { child, .. } =
		nix::unistd::fork().expect("Fork failed")
	{
		child
	} else {
		{
			let _: fn(fn()) = forbid_alloc;
			// Memory can be in a weird state now. Imagine a thread has just taken out a lock,
			// but we've just forked. Lock still held. Avoid deadlock by doing nothing fancy here.
			// Including malloc.

			// println!("{:?}", args[0]);
			#[cfg(any(target_os = "android", target_os = "linux"))]
			{
				let err =
					unsafe { nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL) };
				assert_eq!(err, 0);
			}
			nix::unistd::close(reader).unwrap();
			for fd in (0..1024).filter(|&fd| fd != binary.as_raw_fd() && fd != writer)
			// FdIter::new().unwrap()
			{
				let _ = nix::unistd::close(fd); //.unwrap();
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
			if true {
				// nix::unistd::execve(&args[0], &args, &vars).expect("Failed to fexecve ELF");
				use more_asserts::*;
				use nix::libc;
				#[inline]
				pub fn execve(
					path: &CString, args: &[CString], mut args_p: Vec<*const libc::c_char>,
					env: &[CString], mut env_p: Vec<*const libc::c_char>,
				) -> nix::Result<()> {
					fn to_exec_array(args: &[CString], args_p: &mut Vec<*const libc::c_char>) {
						for arg in args.iter().map(|s| s.as_ptr()) {
							args_p.push(arg);
						}
						args_p.push(std::ptr::null());
					}
					assert_eq!(args_p.len(), 0);
					assert_eq!(env_p.len(), 0);
					assert_le!(args.len() + 1, args_p.capacity());
					assert_le!(env.len() + 1, env_p.capacity());
					to_exec_array(args, &mut args_p);
					to_exec_array(env, &mut env_p);

					let _ = unsafe { libc::execve(path.as_ptr(), args_p.as_ptr(), env_p.as_ptr()) };

					Err(nix::Error::Sys(nix::errno::Errno::last()))
				}
				execve(&args[0], &args, args_p, &vars, vars_p)
					.expect("Failed to execve /proc/self/exe"); // or fexecve but on linux that uses proc also
			} else {
				// if is_valgrind() {
				// 	let binary_desired_fd_ = valgrind_start_fd()-1; assert!(binary_desired_fd_ > binary_desired_fd);
				// 	nix::unistd::dup2(binary_desired_fd, binary_desired_fd_).unwrap();
				// 	nix::unistd::close(binary_desired_fd).unwrap();
				// 	binary_desired_fd = binary_desired_fd_;
				// }
				fexecve(4, &args, &vars).expect("Failed to fexecve ELF");
			}
			unreachable!()
		}
	};
	nix::unistd::close(writer).unwrap();
	// let _ = thread::Builder::new()
	// 	.name(String::from(""))
	// 	.spawn(move || {
	// 		thread::sleep(RECCE_TIMEOUT);
	// 		let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
	// 	})
	// 	.unwrap();
	// TODO: do this without waitpid/kill race
	loop {
		match nix::sys::wait::waitpid(child, None) {
			Err(nix::Error::Sys(nix::errno::Errno::EINTR)) => (),
			Ok(nix::sys::wait::WaitStatus::Exited(pid, code)) if code == 0 => {
				assert_eq!(pid, child);
				break;
			}
			Ok(nix::sys::wait::WaitStatus::Signaled(pid, signal, _))
				if signal == nix::sys::signal::Signal::SIGKILL =>
			{
				assert_eq!(pid, child);
				break;
			}
			wait_status => panic!("{:?}", wait_status),
		}
	}
	let reader = unsafe { File::from_raw_fd(reader) };
	bincode::deserialize_from(&mut &reader)
		.map_err(map_bincode_err)
		.map_err(|_| ())
}

fn manage_connection(
	stream: TcpStream,
	sender: mpsc::SyncSender<(FabricRequest<Vec<u8>, File>, mpsc::SyncSender<Option<Pid>>)>,
) -> Result<(), ()> {
	#[cfg(not(any(target_os = "macos", target_os = "ios")))]
	nix::sys::socket::setsockopt(
		stream.as_raw_fd(),
		nix::sys::socket::sockopt::Linger,
		&nix::libc::linger {
			l_onoff: 1,
			l_linger: 10,
		},
	)
	.map_err(drop)?;
	#[cfg(any(target_os = "macos", target_os = "ios"))]
	{
		use std::convert::TryInto;
		const SO_LINGER_SEC: nix::libc::c_int = 0x1080;
		let res = unsafe {
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
		nix::errno::Errno::result(res).map(drop).map_err(drop)?;
	}
	let (mut stream_read, mut stream_write) = (BufferedStream::new(&stream), &stream);
	let (resources, args, vars, binary, mut arg) = parse_request(&mut stream_read).map_err(drop)?;
	// let request: FabricRequest::<Vec<u8>,File> = bincode_deserialize_from(&mut stream).map_err(map_bincode_err).map_err(drop)?;
	assert_eq!(arg.len(), 0);
	bincode::serialize_into(&mut arg, &constellation::pid()).unwrap();
	let resources = resources.or_else(|| recce(&binary, &args, &vars).ok());
	let pid: Option<Pid> = resources.and_then(|resources| {
		let (sender_, receiver) = mpsc::sync_channel(0);
		sender
			.send((
				FabricRequest {
					resources,
					bind: vec![],
					args,
					vars,
					arg,
					binary,
				},
				sender_,
			))
			.unwrap();
		receiver.recv().unwrap()
	});
	bincode::serialize_into(&mut stream_write, &pid).map_err(drop)?;
	if let Some(pid) = pid {
		let x = PROCESS_COUNT.fetch_add(1, atomic::Ordering::Relaxed);
		trace!("BRIDGE: SPAWN ({})", x);
		let (sender, receiver) = mpsc::sync_channel(0);
		let sender_clone = sender.clone();
		let (sender1, receiver1) = futures::channel::mpsc::channel(0);
		let _ = thread::Builder::new()
			.name(String::from("c"))
			.spawn(move || monitor_process(pid, sender, receiver1))
			.unwrap();
		let hashmap = &Mutex::new(HashMap::new());
		let _ = hashmap.lock().unwrap().insert(pid, sender1);
		crossbeam::scope(|scope| {
			let _ = scope.spawn(move |_scope| loop {
				let event: Result<DeployInputEvent, _> =
					bincode::deserialize_from(&mut stream_read).map_err(map_bincode_err);
				let is_err = event.is_err();
				sender_clone.send(Either::Right(event.ok())).unwrap();
				if is_err {
					break;
				}
			});
			for event in receiver.iter() {
				match event {
					Either::Left(event) => {
						let event = match event {
							OutputEventInt::Spawn(pid, new_pid, sender) => {
								let x = PROCESS_COUNT.fetch_add(1, atomic::Ordering::Relaxed);
								trace!("BRIDGE: SPAWN ({})", x);
								let x = hashmap.lock().unwrap().insert(new_pid, sender);
								assert!(x.is_none());
								DeployOutputEvent::Spawn(pid, new_pid)
							}
							OutputEventInt::Output(pid, fd, output) => {
								DeployOutputEvent::Output(pid, fd, output)
							}
							OutputEventInt::Exit(pid, exit_code) => {
								let x = PROCESS_COUNT.fetch_sub(1, atomic::Ordering::Relaxed);
								assert_ne!(x, 0);
								trace!("BRIDGE: KILL ({})", x);
								let _ = hashmap.lock().unwrap().remove(&pid).unwrap();
								DeployOutputEvent::Exit(pid, exit_code)
							}
						};
						if bincode::serialize_into(&mut stream_write, &event).is_err() {
							break;
						}
					}
					Either::Right(event) => match event {
						Some(DeployInputEvent::Input(pid, fd, input)) => {
							if let Some(sender) = hashmap.lock().unwrap().get_mut(&pid) {
								let _unchecked_error = futures::executor::block_on(
									sender.send(InputEventInt::Input(fd, input)),
								);
							}
						}
						Some(DeployInputEvent::Kill(Some(pid))) => {
							if let Some(sender) = hashmap.lock().unwrap().get_mut(&pid) {
								let _unchecked_error =
									futures::executor::block_on(sender.send(InputEventInt::Kill));
							}
						}
						Some(DeployInputEvent::Kill(None)) | None => {
							break;
						}
					},
				}
			}
			trace!("BRIDGE: KILLED: {:?}", *hashmap.lock().unwrap());
			let mut x = hashmap.lock().unwrap();
			for (_, mut process) in x.drain() {
				let _unchecked_error =
					futures::executor::block_on(process.send(InputEventInt::Kill));
			}
			for _event in receiver {}
		})
		.unwrap();
		assert_eq!(hashmap.lock().unwrap().len(), 0);
	}
	nix::sys::socket::shutdown(stream.as_raw_fd(), nix::sys::socket::Shutdown::Write)
		.map_err(drop)?;
	drop((stream, sender));
	Ok(())
}

fn main() {
	let listener = constellation::bridge_init();
	trace!("BRIDGE: Resources: {:?}", ()); // TODO
	let (sender, receiver) = mpsc::sync_channel(0);
	let _ = thread::Builder::new()
		.name(String::from("a"))
		.spawn(move || {
			for stream in listener.incoming() {
				trace!("BRIDGE: accepted");
				let sender = sender.clone();
				let _ = thread::Builder::new()
					.name(String::from("b"))
					.spawn(|| manage_connection(stream.unwrap(), sender))
					.unwrap();
			}
		})
		.unwrap();

	for (request, sender) in receiver {
		let scheduler = unsafe { File::from_raw_fd(SCHEDULER_FD) };
		let (mut scheduler_read, mut scheduler_write) = (
			BufferedStream::new(&scheduler),
			BufferedStream::new(&scheduler),
		);

		bincode_serialize_into(&mut scheduler_write.write(), &request).unwrap();

		let pid: Option<Pid> = bincode::deserialize_from(&mut scheduler_read)
			.map_err(map_bincode_err)
			.unwrap();
		sender.send(pid).unwrap();
		drop((scheduler_read, scheduler_write));
		let _ = scheduler.into_raw_fd();
	}
}
