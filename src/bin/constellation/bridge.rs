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
	clippy::shadow_unrelated,
	clippy::too_many_lines
)]

use futures::{future::FutureExt, sink::SinkExt, stream::StreamExt};
use log::trace;
use nix::sys::signal::Signal;
use palaver::{
	file::{execve, fexecve, move_fds}, process::WaitStatus
};
use std::{
	collections::HashMap, ffi::{CStr, CString, OsString}, fs::File, iter, net::TcpStream, os::unix::{
		ffi::OsStringExt, io::{AsRawFd, FromRawFd, IntoRawFd}
	}, sync::{
		atomic::{self, AtomicUsize}, mpsc, Arc, Mutex
	}, thread, time::Duration
};

use constellation::FutureExt1;
use constellation_internal::{
	abort_on_unwind, abort_on_unwind_1, forbid_alloc, map_bincode_err, msg::{
		bincode_deserialize_from, bincode_serialize_into, BridgeRequest, FabricRequest, SpawnArg
	}, BufferedStream, DeployInputEvent, DeployOutputEvent, ExitStatus, Fd, Pid, ProcessInputEvent, ProcessOutputEvent, Resources, TrySpawnError
};

const SCHEDULER_FD: Fd = 4;
const RECCE_TIMEOUT: Duration = Duration::from_secs(10); // Time to allow binary to call init()

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

static PROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);

fn monitor_process(
	pid: Pid, sender_: mpsc::SyncSender<OutputEventInt>,
	mut receiver_: futures::channel::mpsc::Receiver<InputEventInt>,
) {
	let receiver = constellation::Receiver::new(pid);
	let sender = constellation::Sender::new(pid);
	loop {
		let x = match futures::future::select(receiver_.next(), receiver.recv().boxed_local())
			.block()
		{
			futures::future::Either::Left((a, _)) => futures::future::Either::Left(a),
			futures::future::Either::Right((a, _)) => futures::future::Either::Right(a),
		};
		match x {
			futures::future::Either::Left(event) => {
				sender
					.send(match event.unwrap() {
						InputEventInt::Input(fd, input) => ProcessInputEvent::Input(fd, input),
						InputEventInt::Kill => ProcessInputEvent::Kill,
					})
					.block();
			}
			futures::future::Either::Right(event) => match event.unwrap() {
				ProcessOutputEvent::Spawn(new_pid) => {
					let (sender1, receiver1) = futures::channel::mpsc::channel(0);
					sender_
						.send(OutputEventInt::Spawn(pid, new_pid, sender1))
						.unwrap();
					let sender_ = sender_.clone();
					let _ = thread::Builder::new()
						.name(String::from("d"))
						.spawn(abort_on_unwind(move || {
							monitor_process(new_pid, sender_, receiver1)
						}))
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
			},
		}
	}
	drop(sender_); // placate clippy needless_pass_by_value
}

fn recce(
	#[cfg(feature = "distribute_binaries")] binary: &File, args: &[OsString],
	vars: &[(OsString, OsString)],
) -> Result<Resources, ()> {
	let (reader, writer) = nix::unistd::pipe().unwrap();

	let args = args
		.iter()
		.map(|x| CString::new(OsStringExt::into_vec(x.clone())).unwrap())
		.collect::<Vec<_>>();
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

	let args: Vec<&CStr> = args.iter().map(|x| &**x).collect();
	let vars: Vec<&CStr> = vars.iter().map(|x| &**x).collect();

	let child = if let palaver::process::ForkResult::Parent(child) =
		palaver::process::fork(false).expect("Fork failed")
	{
		child
	} else {
		forbid_alloc(|| {
			// Memory can be in a weird state now. Imagine a thread has just taken out a lock,
			// but we've just forked. Lock still held. Avoid deadlock by doing nothing fancy here.
			// Including malloc.

			// println!("{:?}", args[0]);
			nix::unistd::close(reader).unwrap();
			// FdIter::new().unwrap()
			for fd in (0..1024).filter(|&fd| {
				#[cfg(feature = "distribute_binaries")]
				let not_binary = fd != binary.as_raw_fd();
				#[cfg(not(feature = "distribute_binaries"))]
				let not_binary = true;
				not_binary && fd != writer
			}) {
				let _ = nix::unistd::close(fd); //.unwrap();
			}
			move_fds(
				&mut [
					(writer, 3),
					#[cfg(feature = "distribute_binaries")]
					(binary.as_raw_fd(), 4),
				],
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
			if cfg!(feature = "distribute_binaries") {
				// if is_valgrind() {
				// 	let binary_desired_fd_ = valgrind_start_fd()-1; assert!(binary_desired_fd_ > binary_desired_fd);
				// 	nix::unistd::dup2(binary_desired_fd, binary_desired_fd_).unwrap();
				// 	nix::unistd::close(binary_desired_fd).unwrap();
				// 	binary_desired_fd = binary_desired_fd_;
				// }
				fexecve(4, &args, &vars).expect("Failed to fexecve for recce");
			} else {
				execve(&args[0], &args, &vars).expect("Failed to execve for recce");
			}
			unreachable!()
		})
	};
	nix::unistd::close(writer).unwrap();
	let child = Arc::new(child);
	let child1 = child.clone();
	let _ = thread::Builder::new()
		.name(String::from(""))
		.spawn(abort_on_unwind(move || {
			thread::sleep(RECCE_TIMEOUT);
			let _ = child1.signal(Signal::SIGKILL);
		}))
		.unwrap();
	match child.wait() {
		Ok(WaitStatus::Exited(0)) => (),
		wait_status => {
			if let Ok(WaitStatus::Signaled(Signal::SIGKILL, _)) = wait_status {
			} else if cfg!(feature = "strict") {
				panic!("{:?}", wait_status)
			}
			return Err(());
		}
	}
	drop(child);
	// TODO: kill timeout and drop(Arc::try_unwrap(child).unwrap());
	let reader = unsafe { File::from_raw_fd(reader) };
	bincode::deserialize_from(&mut &reader).map_err(|_| ())
}

fn manage_connection(
	stream: TcpStream,
	sender: mpsc::SyncSender<(
		FabricRequest<Vec<u8>, File>,
		mpsc::SyncSender<Result<Pid, TrySpawnError>>,
	)>,
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
	let mut request: BridgeRequest<Vec<u8>, File> = bincode_deserialize_from(&mut stream_read)
		.map_err(map_bincode_err)
		.map_err(drop)?;
	assert_eq!(request.arg.len(), 0);
	let spawn_arg = SpawnArg::<()> {
		bridge: constellation::pid(),
		spawn: None,
	};
	bincode::serialize_into(&mut request.arg, &spawn_arg).unwrap();
	let resources = request
		.resources
		.clone()
		.or_else(|| {
			recce(
				#[cfg(feature = "distribute_binaries")]
				&request.binary,
				&request.args,
				&request.vars,
			)
			.ok()
		})
		.ok_or(TrySpawnError::Recce);
	let pid: Result<Pid, TrySpawnError> = resources.and_then(|resources| {
		let (sender_, receiver) = mpsc::sync_channel(0);
		sender
			.send((
				FabricRequest {
					block: true, // use cases for false?
					resources,
					bind: vec![],
					args: request.args,
					vars: request.vars,
					arg: request.arg,
					binary: request.binary,
				},
				sender_,
			))
			.unwrap();
		receiver.recv().unwrap()
	});
	bincode::serialize_into(&mut stream_write, &pid).map_err(drop)?;
	if let Ok(pid) = pid {
		let x = PROCESS_COUNT.fetch_add(1, atomic::Ordering::Relaxed);
		trace!("BRIDGE: SPAWN ({})", x);
		let (sender, receiver) = mpsc::sync_channel(0);
		let (sender1, receiver1) = futures::channel::mpsc::channel(0);
		let _ = thread::Builder::new()
			.name(String::from("c"))
			.spawn(abort_on_unwind(move || {
				monitor_process(pid, sender, receiver1)
			}))
			.unwrap();
		let hashmap = &Mutex::new(HashMap::new());
		let _ = hashmap.lock().unwrap().insert(pid, sender1);
		crossbeam::scope(|scope| {
			let _ = scope.spawn(abort_on_unwind_1(move |_scope| {
				loop {
					let event: Result<DeployInputEvent, _> =
						bincode::deserialize_from(&mut stream_read).map_err(map_bincode_err);
					if event.is_err() {
						break;
					}
					match event.unwrap() {
						DeployInputEvent::Input(pid, fd, input) => {
							if let Some(sender) = hashmap.lock().unwrap().get_mut(&pid) {
								let _unchecked_error =
									sender.send(InputEventInt::Input(fd, input)).block();
							}
						}
						DeployInputEvent::Kill(Some(pid)) => {
							if let Some(sender) = hashmap.lock().unwrap().get_mut(&pid) {
								let _unchecked_error = sender.send(InputEventInt::Kill).block();
							}
						}
						DeployInputEvent::Kill(None) => {
							break;
						}
					}
				}
				let mut x = hashmap.lock().unwrap();
				for (_, process) in x.iter_mut() {
					let _unchecked_error = process.send(InputEventInt::Kill).block();
				}
			}));
			for event in receiver.iter() {
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
			trace!("BRIDGE: KILLED: {:?}", *hashmap.lock().unwrap());
			let mut x = hashmap.lock().unwrap();
			for (_, mut process) in x.drain() {
				let _unchecked_error = process.send(InputEventInt::Kill).block();
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

pub fn main() {
	let listener = constellation::bridge_init();
	trace!("BRIDGE: Resources: {:?}", ()); // TODO
	let (sender, receiver) = mpsc::sync_channel(0);
	let _ = thread::Builder::new()
		.name(String::from("a"))
		.spawn(abort_on_unwind(move || {
			for stream in listener.incoming() {
				if stream.is_err() {
					continue;
				}
				trace!("BRIDGE: accepted");
				let sender = sender.clone();
				let _ = thread::Builder::new()
					.name(String::from("b"))
					.spawn(abort_on_unwind(|| {
						manage_connection(stream.unwrap(), sender)
					}))
					.unwrap();
			}
		}))
		.unwrap();

	for (request, sender) in receiver {
		let scheduler = unsafe { File::from_raw_fd(SCHEDULER_FD) };
		let (mut scheduler_read, mut scheduler_write) = (
			BufferedStream::new(&scheduler),
			BufferedStream::new(&scheduler),
		);

		bincode_serialize_into(&mut scheduler_write.write(), &request).unwrap();

		let pid: Result<Pid, TrySpawnError> = bincode::deserialize_from(&mut scheduler_read)
			.map_err(map_bincode_err)
			.unwrap();
		sender.send(pid).unwrap();
		drop((scheduler_read, scheduler_write));
		let _ = scheduler.into_raw_fd();
	}
}
