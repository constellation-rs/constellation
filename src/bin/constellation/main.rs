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
	clippy::too_many_lines
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
	file::{copy_fd, execve, fexecve, move_fds}, socket::{socket, SockFlag}, valgrind
};
use std::{
	collections::HashMap, convert::{TryFrom, TryInto}, env, io, io::Seek, net::{IpAddr, SocketAddr, TcpListener}, process, sync, thread
};
#[cfg(unix)]
use std::{
	ffi::{CStr, CString}, fs::File, os::unix::{ffi::OsStringExt, io::IntoRawFd}
};

#[cfg(feature = "kubernetes")]
use self::kube::kube_master;
use constellation_internal::{
	abort_on_unwind, abort_on_unwind_1, forbid_alloc, map_bincode_err, msg::{bincode_deserialize_from, FabricRequest}, BufferedStream, FabricOutputEvent, Fd, Format, Pid, PidInternal, Trace
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
		mem: u64,
		cpu: u32,
		replicas: u32,
	},
	Master(SocketAddr, Vec<Node>),
	Worker(SocketAddr),
	Bridge,
}
#[derive(PartialEq, Debug)]
struct Node {
	fabric: SocketAddr,
	bridge: Option<SocketAddr>,
	mem: u64,
	cpu: u32,
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
		eprintln!("{:?}", backtrace::Backtrace::new());
		std::process::abort();
	}));
	// simple_logging::log_to_file(
	// 	format!("logs/{}.log", std::process::id()),
	// 	log::LevelFilter::Trace,
	// )
	// .unwrap();
	let args = Args::from_args(env::args().skip(1)).unwrap_or_else(|(message, success)| {
		println!("{}", message);
		process::exit(if success { 0 } else { 1 })
	});
	if args.role == Role::Bridge {
		return bridge::main();
	}
	let stdout = io::stdout();
	let trace = &Trace::new(stdout, args.format, args.verbose);
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
		Role::Master(listen, mut nodes) => {
			let fabric = TcpListener::bind(SocketAddr::new(listen.ip(), 0)).unwrap();
			let master_addr = nodes[0].fabric;
			nodes[0]
				.fabric
				.set_port(fabric.local_addr().unwrap().port());
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
			let _ = thread::Builder::new()
				.name(String::from("master"))
				.spawn(abort_on_unwind(move || {
					std::thread::sleep(std::time::Duration::from_secs(1));
					master::run(
						SocketAddr::new(listen.ip(), master_addr.port()),
						Pid::new(master_addr.ip(), master_addr.port()),
						nodes,
					)
				}))
				.unwrap();
			(listen.ip(), fabric)
		}
		Role::Worker(listen) => (listen.ip(), TcpListener::bind(&listen).unwrap()),
		Role::Bridge => unreachable!(),
	};

	loop {
		let accepted = listener.accept();
		if accepted.is_err() {
			continue;
		}
		let (stream, addr) = accepted.unwrap();
		let mut pending_inner = HashMap::new();
		let pending = &sync::RwLock::new(&mut pending_inner);
		let (mut stream_read, stream_write) =
			(BufferedStream::new(&stream), &sync::Mutex::new(&stream));
		if bincode::serialize_into::<_, IpAddr>(&mut *stream_write.try_lock().unwrap(), &addr.ip())
			.is_err()
		{
			continue;
		}
		let ip = bincode::deserialize_from::<_, IpAddr>(&mut stream_read);
		if ip.is_err() {
			continue;
		}
		let ip = ip.unwrap();
		crossbeam::scope(|scope| {
			while let Ok(request) =
				bincode_deserialize_from(&mut stream_read).map_err(map_bincode_err)
			{
				let request: FabricRequest<File, File> = request;
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
					&socket::SockAddr::Inet(socket::InetAddr::from_std(&SocketAddr::new(
						listen, 0,
					))),
				)
				.unwrap();
				socket::setsockopt(process_listener, socket::sockopt::ReusePort, &true).unwrap();
				let port = if let socket::SockAddr::Inet(inet) =
					socket::getsockname(process_listener).unwrap()
				{
					inet.to_std()
				} else {
					panic!()
				}
				.port();
				let process_id = Pid::new(ip, port);
				let _ = (&request.arg).seek(std::io::SeekFrom::End(0)).unwrap();
				bincode::serialize_into(&request.arg, &process_id).unwrap();
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
				.chain(request.vars.into_iter().map(|(x, y)| {
					(
						CString::new(OsStringExt::into_vec(x)).unwrap(),
						CString::new(OsStringExt::into_vec(y)).unwrap(),
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

				#[cfg(feature = "distribute_binaries")]
				let binary = request.binary;
				let mut binary_desired_fd =
					BOUND_FD_START + Fd::try_from(request.bind.len()).unwrap();
				let arg = request.arg;
				let bind = request.bind;

				let child = match palaver::process::fork(false).expect("Fork failed") {
					palaver::process::ForkResult::Child => {
						forbid_alloc(|| {
							// Memory can be in a weird state now. Imagine a thread has just taken out a lock,
							// but we've just forked. Lock still held. Avoid deadlock by doing nothing fancy here.
							// Including malloc.

							// println!("{:?}", args[0]);
							unistd::setpgid(unistd::Pid::from_raw(0), unistd::Pid::from_raw(0))
								.unwrap();
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
									copy_fd(fd, socket, Some(fcntl::FdFlag::empty()), true)
										.unwrap();
									unistd::close(fd).unwrap();
								}
								socket::setsockopt(socket, socket::sockopt::ReuseAddr, &true)
									.unwrap();
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
									copy_fd(
										binary_desired_fd,
										binary_desired_fd_,
										Some(fcntl::FdFlag::empty()),
										true,
									)
									.unwrap();
									unistd::close(binary_desired_fd).unwrap();
									binary_desired_fd = binary_desired_fd_;
								}
								fexecve(binary_desired_fd, &args, &vars)
									.expect("Failed to fexecve for fabric");
							} else {
								execve(&args[0], &args, &vars)
									.expect("Failed to execve for fabric");
							}
							unreachable!()
						})
					}
					palaver::process::ForkResult::Parent(child) => child,
				};
				unistd::close(process_listener).unwrap();
				let x = pending.write().unwrap().insert(process_id, ());
				assert!(x.is_none());
				trace.fabric(FabricOutputEvent::Init {
					pid: process_id,
					system_pid: nix::libc::pid_t::from(child.pid).try_into().unwrap(),
				});
				if bincode::serialize_into(
					*stream_write.lock().unwrap(),
					&Either::Left::<Pid, Pid>(process_id),
				)
				.map_err(map_bincode_err)
				.is_err()
				{
					break;
				}
				let _ = scope.spawn(abort_on_unwind_1(move |_scope| {
					let child_pid = child.pid;
					match child.wait() {
						Ok(palaver::process::WaitStatus::Exited(0))
						| Ok(palaver::process::WaitStatus::Signaled(signal::Signal::SIGKILL, _)) => (),
						wait_status => panic!("{:?}", wait_status),
					}
					pending.write().unwrap().remove(&process_id).unwrap();
					trace.fabric(FabricOutputEvent::Exit {
						pid: process_id,
						system_pid: nix::libc::pid_t::from(child_pid).try_into().unwrap(),
					});
					let _unchecked_error = bincode::serialize_into(
						*stream_write.lock().unwrap(),
						&Either::Right::<Pid, Pid>(process_id),
					)
					.map_err(map_bincode_err);
				}));
			}
			for (&_job, _pid) in pending.read().unwrap().iter() {
				// TODO: this is racey
				// let _unchecked_error = pid.signal(signal::Signal::SIGKILL);
			}
		})
		.unwrap();
		assert_eq!(pending_inner.len(), 0);
	}
}
