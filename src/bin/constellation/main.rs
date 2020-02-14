//! This is the constellation binary that, when run one or more times, constitutes a constellation cluster.
//!
//! # `constellation`
//! Run a constellation node, optionally as master, and optionally start a bridge running
//!
//! ## Usage
//! ```text
//! constellation master (<addr> <mem> <cpu> [<bridge> <addr>]...)...
//! constellation <addr>
//! ```
//!
//! ## Options
//! ```text
//! -h --help          Show this screen.
//! -V --version       Show version.
//! ```
//!
//! A constellation cluster comprises one or more nodes, where one is declared master.
//!
//! The arguments to the master node are the address and resources of each node,
//! including itself. The arguments for a node can include a binary to spawn
//! immediately and an address to reserve for it. This is intended to be used to
//! spawn the bridge, which works with the deploy command and library to handle
//! transparent capture and forwarding of output and debug information.
//!
//! The first set of arguments to the master node is for itself – as such the
//! address is bound to not connected to.
//!
//! The argument to non-master workers is the address to bind to.
//!
//! For example, respective invocations on each of a cluster of 3 servers with
//! 512GiB memory and 36 logical cores apiece might be:
//! ```text
//! constellation 10.0.0.2:9999
//! ```
//! ```text
//! constellation 10.0.0.3:9999
//! ```
//! ```text
//! constellation master 10.0.0.1:9999 400GiB 34 bridge 10.0.0.1:8888 \
//!               10.0.0.2:9999 400GiB 34 \
//!               10.0.0.3:9999 400GiB 34
//! ```
//!
//! Deploying to this cluster might then be:
//! ```text
//! deploy 10.0.0.1:8888 ./binary
//! ```
//! or, for a Rust crate:
//! ```text
//! cargo deploy 10.0.0.1:8888
//! ```

#![feature(backtrace)]
#![warn(
	// missing_copy_implementations,
	missing_debug_implementations,
	missing_docs,
	trivial_numeric_casts,
	unused_extern_crates,
	unused_import_braces,
	unused_qualifications,
	unused_results,
	clippy::pedantic,
)] // from https://github.com/rust-unofficial/patterns/blob/master/anti_patterns/deny-warnings.md
#![allow(
	dead_code,
	clippy::similar_names,
	clippy::type_complexity,
	clippy::non_ascii_literal,
	clippy::shadow_unrelated,
	clippy::too_many_lines,
	clippy::erasing_op
)]

mod args;
mod bridge;
#[cfg(feature = "kubernetes")]
mod kube;
mod master;

use either::Either;
#[cfg(unix)]
use nix::{fcntl, sys::signal, sys::socket, unistd};
use palaver::{
	file::{execve, fexecve, move_fd, move_fds}, process::ChildHandle, socket::{socket, SockFlag}, valgrind
};
use std::{
	collections::HashMap, convert::{TryFrom, TryInto}, env, io, io::Seek, net::{IpAddr, SocketAddr, TcpListener}, process, sync, sync::Arc, thread
};
#[cfg(unix)]
use std::{
	ffi::{CStr, CString, OsString}, fs::File, os::unix::{ffi::OsStringExt, io::IntoRawFd}
};

#[cfg(feature = "kubernetes")]
use self::kube::kube_master;
use constellation_internal::{
	abort_on_unwind_1, file_from_reader, forbid_alloc, map_bincode_err, msg::{bincode_deserialize_from, FabricRequest}, BufferedStream, Cpu, FabricOutputEvent, Fd, Format, Mem, Pid, PidInternal, Resources, Trace
};

#[derive(PartialEq, Debug)]
struct Args {
	format: Format,
	verbose: bool,
	role: Role,
}
#[derive(PartialEq, Debug)]
enum Role {
	#[cfg(feature = "kubernetes")]
	KubeMaster {
		master_bind: SocketAddr,
		bridge_bind: SocketAddr,
		mem: Mem,
		cpu: Cpu,
		replicas: u32,
	},
	Master(SocketAddr, Option<u128>, Vec<Node>),
	Master2(IpAddr, SocketAddr, Option<u128>, Vec<Node>),
	Worker(SocketAddr, Option<u128>),
	Bridge,
}
#[derive(PartialEq, Debug)]
struct Node {
	fabric: SocketAddr,
	bridge: Option<SocketAddr>,
	mem: Mem,
	cpu: Cpu,
}

const LISTENER_FD: Fd = 3;
const ARG_FD: Fd = 4;
const BOUND_FD_START: Fd = 5;

fn main() {
	std::env::set_var("RUST_BACKTRACE", "full");
	std::panic::set_hook(Box::new(|info| {
		eprintln!(
			"thread '{}' {}",
			thread::current().name().unwrap_or("<unnamed>"),
			info
		);
		eprintln!("{:?}", std::backtrace::Backtrace::force_capture());
		std::process::abort();
	}));
	let args = Args::from_args(env::args().skip(1)).unwrap_or_else(|(message, success)| {
		println!("{}", message);
		process::exit(if success { 0 } else { 1 })
	});
	if args.role == Role::Bridge {
		return bridge::main();
	}
	if let Role::Master2(listen, master_addr, key, nodes) = args.role {
		let nodes = nodes
			.into_iter()
			.map(
				|Node {
				     fabric,
				     bridge,
				     mem,
				     cpu,
				 }| { (fabric, (bridge, mem, cpu)) },
			)
			.collect::<HashMap<_, _>>(); // TODO: error on clash
		master::run(
			SocketAddr::new(listen, master_addr.port()),
			Pid::new_with(master_addr.ip(), master_addr.port(), key),
			nodes,
		)
	}
	let trace = &Trace::new(io::stdout(), args.format, args.verbose);
	let mut master = None;
	let (listen, listener) = match args.role {
		#[cfg(feature = "kubernetes")]
		Role::KubeMaster {
			master_bind,
			bridge_bind,
			mem,
			cpu,
			replicas,
		} => {
			let fabric = TcpListener::bind(SocketAddr::new(master_bind.ip(), 0)).unwrap();
			kube_master(
				master_bind,
				fabric.local_addr().unwrap().port(),
				bridge_bind,
				mem,
				cpu,
				replicas,
			);
			(master_bind.ip(), fabric)
		}
		Role::Master(listen, key, mut nodes) => {
			let fabric = TcpListener::bind(SocketAddr::new(listen.ip(), 0)).unwrap();
			let master_addr = nodes[0].fabric;
			nodes[0]
				.fabric
				.set_port(fabric.local_addr().unwrap().port());
			master = Some((listen.ip(), master_addr, key, nodes));
			(listen.ip(), fabric)
		}
		Role::Worker(listen, key) => (listen.ip(), TcpListener::bind(&listen).unwrap()),
		Role::Bridge | Role::Master2(_, _, _, _) => unreachable!(),
	};

	loop {
		let mut pending_inner = HashMap::new();
		let pending = &sync::RwLock::new(&mut pending_inner);
		crossbeam::scope(|scope| {
			if let Some((master_listen, master_addr, key, nodes)) = master.take() {
				let mut args = vec![
					OsString::from(env::current_exe().unwrap()),
					OsString::from("master2"),
					OsString::from(master_listen.to_string()),
					OsString::from(master_addr.to_string()),
					OsString::from("-"),
				];
				if let Some(key) = key {
					args.push(OsString::from(key.to_string()));
				}
				for node in &nodes {
					args.extend(
						vec![
							node.fabric.to_string(),
							node.bridge
								.map_or_else(|| String::from("-"), |bridge| bridge.to_string()),
							node.mem.to_string(),
							node.cpu.to_string(),
						]
						.into_iter()
						.map(OsString::from),
					);
				}
				#[cfg(feature = "distribute_binaries")]
				let binary = palaver::env::exe().unwrap();
				#[cfg(not(feature = "distribute_binaries"))]
				let binary = std::marker::PhantomData;
				let arg = file_from_reader(&mut &*vec![], 0, &OsString::from("a"), false).unwrap();
				let request: FabricRequest<File, File> = FabricRequest {
					block: false,
					resources: Resources {
						mem: 0 * Mem::B,
						cpu: 0 * Cpu::CORE,
					},
					bind: vec![],
					args,
					vars: Vec::new(),
					binary,
					arg,
				};
				let (pid, child) = spawn(listen, nodes[0].fabric.ip(), request);
				let child = Arc::new(child);
				let x = pending.write().unwrap().insert(pid, child.clone());
				assert!(x.is_none());
				trace.fabric(FabricOutputEvent::Init {
					pid,
					system_pid: nix::libc::pid_t::from(child.pid).try_into().unwrap(),
				});
				let _ = scope.spawn(abort_on_unwind_1(move |_scope| {
					match child.wait() {
						Ok(palaver::process::WaitStatus::Exited(0))
						| Ok(palaver::process::WaitStatus::Signaled(signal::Signal::SIGKILL, _)) => (),
						wait_status => {
							if cfg!(feature = "strict") {
								panic!("{:?}", wait_status)
							}
						}
					}
					let x = pending.write().unwrap().remove(&pid).unwrap();
					assert!(Arc::ptr_eq(&child, &x));
					drop(x);
					let child = Arc::try_unwrap(child).unwrap();
					let child_pid = child.pid;
					drop(child);
					trace.fabric(FabricOutputEvent::Exit {
						pid,
						system_pid: nix::libc::pid_t::from(child_pid).try_into().unwrap(),
					});
				}));
			}
			let accepted = listener.accept();
			if let Ok((stream, addr)) = accepted {
				let (mut stream_read, stream_write) =
					(BufferedStream::new(&stream), &sync::Mutex::new(&stream));
				crossbeam::scope(|scope| {
					if bincode::serialize_into::<_, IpAddr>(
						&mut *stream_write.try_lock().unwrap(),
						&addr.ip(),
					)
					.is_ok()
					{
						let ip = bincode::deserialize_from::<_, IpAddr>(&mut stream_read);
						if let Ok(ip) = ip {
							while let Ok(request) =
								bincode_deserialize_from(&mut stream_read).map_err(map_bincode_err)
							{
								let request: FabricRequest<File, File> = request;
								let (pid, child) = spawn(listen, ip, request);
								let child = Arc::new(child);
								let x = pending.write().unwrap().insert(pid, child.clone());
								assert!(x.is_none());
								trace.fabric(FabricOutputEvent::Init {
									pid,
									system_pid: nix::libc::pid_t::from(child.pid)
										.try_into()
										.unwrap(),
								});
								if bincode::serialize_into(
									*stream_write.lock().unwrap(),
									&Either::Left::<Pid, Pid>(pid),
								)
								.map_err(map_bincode_err)
								.is_err()
								{
									break;
								}
								let _ = scope.spawn(abort_on_unwind_1(move |_scope| {
									match child.wait() {
										Ok(palaver::process::WaitStatus::Exited(0))
										| Ok(palaver::process::WaitStatus::Signaled(
											signal::Signal::SIGKILL,
											_,
										)) => (),
										wait_status => {
											if cfg!(feature = "strict") {
												panic!("{:?}", wait_status)
											}
										}
									}
									let x = pending.write().unwrap().remove(&pid).unwrap();
									assert!(Arc::ptr_eq(&child, &x));
									drop(x);
									let child = Arc::try_unwrap(child).unwrap();
									let child_pid = child.pid;
									drop(child);
									trace.fabric(FabricOutputEvent::Exit {
										pid,
										system_pid: nix::libc::pid_t::from(child_pid)
											.try_into()
											.unwrap(),
									});
									let _unchecked_error = bincode::serialize_into(
										*stream_write.lock().unwrap(),
										&Either::Right::<Pid, Pid>(pid),
									)
									.map_err(map_bincode_err);
								}));
							}
						}
					}
				})
				.unwrap();
			}
			for (&_pid, child) in pending.read().unwrap().iter() {
				let _unchecked_error = signal::kill(
					nix::unistd::Pid::from_raw(-child.pid.as_raw()),
					signal::Signal::SIGKILL,
				);
				let _unchecked_error = child.signal(signal::Signal::SIGKILL);
			}
		})
		.unwrap();
		assert_eq!(pending_inner.len(), 0);
	}
}

fn spawn(listen: IpAddr, ip: IpAddr, request: FabricRequest<File, File>) -> (Pid, ChildHandle) {
	let process_listener = socket(
		socket::AddressFamily::Inet,
		socket::SockType::Stream,
		SockFlag::SOCK_NONBLOCK,
		socket::SockProtocol::Tcp,
	)
	.unwrap();
	socket::setsockopt(process_listener, socket::sockopt::ReuseAddr, &true).unwrap();
	socket::bind(
		process_listener,
		&socket::SockAddr::Inet(socket::InetAddr::from_std(&SocketAddr::new(listen, 0))),
	)
	.unwrap();
	socket::setsockopt(process_listener, socket::sockopt::ReusePort, &true).unwrap();
	let port = if let socket::SockAddr::Inet(inet) = socket::getsockname(process_listener).unwrap()
	{
		inet.to_std()
	} else {
		panic!()
	}
	.port();
	let pid = Pid::new(ip, port);
	let _ = (&request.arg).seek(std::io::SeekFrom::End(0)).unwrap();
	bincode::serialize_into(&request.arg, &pid).unwrap();
	let _ = (&request.arg).seek(std::io::SeekFrom::Start(0)).unwrap();

	let args = request
		.args
		.into_iter()
		.map(|x| CString::new(OsStringExt::into_vec(x)).unwrap())
		.collect::<Vec<_>>();
	let vars = [
		(
			CString::new("CONSTELLATION").unwrap(),
			CString::new("fabric").unwrap(),
		),
		(
			CString::new("CONSTELLATION_RESOURCES").unwrap(),
			CString::new(serde_json::to_string(&request.resources).unwrap()).unwrap(),
		),
	]
	.iter()
	.cloned()
	.chain(
		request
			.vars
			.into_iter()
			.map(|(x, y)| {
				(
					CString::new(OsStringExt::into_vec(x)).unwrap(),
					CString::new(OsStringExt::into_vec(y)).unwrap(),
				)
			})
			.filter(|&(ref x, _)| {
				x.to_str() != Ok("CONSTELLATION") && x.to_str() != Ok("CONSTELLATION_RESOURCES")
			}),
	)
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

	#[cfg(feature = "distribute_binaries")]
	let binary = request.binary;
	let mut binary_desired_fd = BOUND_FD_START + Fd::try_from(request.bind.len()).unwrap();
	let arg = request.arg;
	let bind = request.bind;

	let child = match palaver::process::fork(false).expect("Fork failed") {
		palaver::process::ForkResult::Child => {
			forbid_alloc(|| {
				// Memory can be in a weird state now. Imagine a thread has just taken out a lock,
				// but we've just forked. Lock still held. Avoid deadlock by doing nothing fancy here.
				// Including malloc.

				// println!("{:?}", args[0]);
				unistd::setpgid(unistd::Pid::from_raw(0), unistd::Pid::from_raw(0)).unwrap();
				#[cfg(feature = "distribute_binaries")]
				let binary = binary.into_raw_fd(); // These are dropped by parent
				let arg = arg.into_raw_fd();
				move_fds(
					&mut [
						(arg, ARG_FD),
						(process_listener, LISTENER_FD),
						#[cfg(feature = "distribute_binaries")]
						(binary, binary_desired_fd),
					],
					Some(fcntl::FdFlag::empty()),
					true,
				);
				for fd in BOUND_FD_START..1024 {
					if cfg!(feature = "distribute_binaries") && fd == binary_desired_fd {
						continue;
					}
					let _ = unistd::close(fd);
				}
				for (i, addr) in bind.iter().enumerate() {
					let socket: Fd = BOUND_FD_START + Fd::try_from(i).unwrap();
					let fd = socket::socket(
						socket::AddressFamily::Inet,
						socket::SockType::Stream,
						socket::SockFlag::empty(),
						socket::SockProtocol::Tcp,
					)
					.unwrap();
					if fd != socket {
						move_fd(fd, socket, Some(fcntl::FdFlag::empty()), true).unwrap();
					}
					socket::setsockopt(socket, socket::sockopt::ReuseAddr, &true).unwrap();
					socket::bind(
						socket,
						&socket::SockAddr::Inet(socket::InetAddr::from_std(&addr)),
					)
					.unwrap();
				}
				if cfg!(feature = "distribute_binaries") {
					if valgrind::is().unwrap_or(false) {
						let binary_desired_fd_ = valgrind::start_fd() - 1;
						assert!(binary_desired_fd_ > binary_desired_fd);
						move_fd(
							binary_desired_fd,
							binary_desired_fd_,
							Some(fcntl::FdFlag::empty()),
							true,
						)
						.unwrap();
						binary_desired_fd = binary_desired_fd_;
					}
					fexecve(binary_desired_fd, &args, &vars).expect("Failed to fexecve for fabric");
				} else {
					execve(&args[0], &args, &vars).expect("Failed to execve for fabric");
				}
				unreachable!()
			})
		}
		palaver::process::ForkResult::Parent(child) => child,
	};
	unistd::close(process_listener).unwrap();
	(pid, child)
}
