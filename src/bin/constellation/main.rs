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
	clippy::shadow_unrelated
)]

mod args;
mod master;

use either::Either;
#[cfg(unix)]
use nix::{fcntl, sys::signal, sys::socket, sys::wait, unistd};
use palaver::{
	file::{copy_fd, fexecve, move_fds}, socket::{socket, SockFlag}, valgrind
};
use std::{
	collections::HashMap, convert::{TryFrom, TryInto}, env, io, iter, net::{self, IpAddr, SocketAddr}, path::PathBuf, process, sync, thread
};
#[cfg(unix)]
use std::{
	ffi::CString, fs::File, os::unix::{ffi::OsStringExt, io::IntoRawFd}
};

use constellation_internal::{
	forbid_alloc, map_bincode_err, msg::{bincode_deserialize_from, FabricRequest}, BufferedStream, FabricOutputEvent, Fd, Format, Pid, PidInternal, Trace
};

#[derive(PartialEq, Debug)]
struct Args {
	format: Format,
	verbose: bool,
	role: Role,
}
#[derive(PartialEq, Debug)]
enum Role {
	Master(IpAddr, Vec<Node>),
	Worker(SocketAddr),
}
#[derive(PartialEq, Debug)]
struct Node {
	addr: SocketAddr,
	mem: u64,
	cpu: f32,
	run: Vec<Run>,
}
#[derive(PartialEq, Debug)]
struct Run {
	binary: PathBuf,
	addr: SocketAddr,
}

const LISTENER_FD: Fd = 3;
const ARG_FD: Fd = 4;
const BOUND_FD_START: Fd = 5;

fn main() {
	std::env::set_var("RUST_BACKTRACE", "full");
	std::panic::set_hook(Box::new(|info| {
		eprintln!("thread '{}' {}", thread::current().name().unwrap(), info);
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
	let stdout = io::stdout();
	let trace = &Trace::new(stdout, args.format, args.verbose);
	let (listen, listener) = match args.role {
		Role::Master(listen, mut nodes) => {
			let fabric = net::TcpListener::bind(SocketAddr::new(listen, 0)).unwrap();
			let master_addr = nodes[0].addr;
			nodes[0].addr.set_port(fabric.local_addr().unwrap().port());
			let _ = thread::Builder::new()
				.name(String::from("master"))
				.spawn(move || {
					master::run(
						SocketAddr::new(listen, master_addr.port()),
						Pid::new(master_addr.ip(), master_addr.port()),
						nodes
							.into_iter()
							.map(
								|Node {
								     addr,
								     mem,
								     cpu,
								     run,
								 }| {
									(
										addr,
										(
											mem,
											cpu,
											run.into_iter()
												.map(|Run { binary, addr }| (binary, vec![addr]))
												.collect(),
										),
									)
								},
							)
							.collect::<HashMap<_, _>>(),
					); // TODO: error on clash
				})
				.unwrap();
			(listen, fabric)
		}
		Role::Worker(listen) => (listen.ip(), net::TcpListener::bind(&listen).unwrap()),
	};

	for stream in listener.incoming() {
		let stream = stream.unwrap();
		// println!("accepted");
		let mut pending_inner = HashMap::new();
		let pending = &sync::RwLock::new(&mut pending_inner);
		let (mut stream_read, stream_write) =
			(BufferedStream::new(&stream), &sync::Mutex::new(&stream));
		let ip = bincode::deserialize_from::<_, IpAddr>(&mut stream_read).unwrap();
		crossbeam::scope(|scope| {
			while let Ok(FabricRequest::<File, File> {
				resources,
				bind: ports,
				args,
				vars,
				arg,
				binary,
			}) = bincode_deserialize_from(&mut stream_read).map_err(map_bincode_err)
			{
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
				let vars = iter::once((
					CString::new("CONSTELLATION").unwrap(),
					CString::new("fabric").unwrap(),
				))
				.chain(iter::once((
					CString::new("CONSTELLATION_RESOURCES").unwrap(),
					CString::new(serde_json::to_string(&resources).unwrap()).unwrap(),
				)))
				.chain(vars.into_iter().map(|(x, y)| {
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
				// let path = CString::new(OsStringExt::into_vec(args[0].clone())).unwrap();
				let args = args
					.into_iter()
					.map(|x| CString::new(OsStringExt::into_vec(x)).unwrap())
					.collect::<Vec<_>>();

				let args_p = Vec::with_capacity(args.len() + 1);
				let vars_p = Vec::with_capacity(vars.len() + 1);

				let child = match unistd::fork().expect("Fork failed") {
					unistd::ForkResult::Child => {
						forbid_alloc(|| {
							// Memory can be in a weird state now. Imagine a thread has just taken out a lock,
							// but we've just forked. Lock still held. Avoid deadlock by doing nothing fancy here.
							// Including malloc.

							// println!("{:?}", args[0]);
							#[cfg(any(target_os = "android", target_os = "linux"))]
							{
								use nix::libc;
								let err =
									unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
								assert_eq!(err, 0);
							}
							unistd::setpgid(unistd::Pid::from_raw(0), unistd::Pid::from_raw(0))
								.unwrap();
							let binary = binary.into_raw_fd();
							let mut binary_desired_fd =
								BOUND_FD_START + Fd::try_from(ports.len()).unwrap();
							let arg = arg.into_raw_fd();
							move_fds(
								&mut [
									(arg, ARG_FD),
									(process_listener, LISTENER_FD),
									(binary, binary_desired_fd),
								],
								Some(fcntl::FdFlag::empty()),
								true,
							);
							for (i, port) in ports.iter().enumerate() {
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
									&socket::SockAddr::Inet(socket::InetAddr::from_std(&port)),
								)
								.unwrap();
							}
							if true {
								// unistd::execve(&path, &args, &vars).expect("Failed to fexecve");
								use more_asserts::*;
								use nix::libc;
								#[inline]
								pub fn execve(
									path: &CString, args: &[CString],
									mut args_p: Vec<*const libc::c_char>, env: &[CString],
									mut env_p: Vec<*const libc::c_char>,
								) -> nix::Result<()> {
									fn to_exec_array(
										args: &[CString], args_p: &mut Vec<*const libc::c_char>,
									) {
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

									let _ = unsafe {
										libc::execve(path.as_ptr(), args_p.as_ptr(), env_p.as_ptr())
									};

									Err(nix::Error::Sys(nix::errno::Errno::last()))
								}
								execve(&args[0], &args, args_p, &vars, vars_p)
									.expect("Failed to execve /proc/self/exe"); // or fexecve but on linux that uses proc also
							} else {
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
									.expect("Failed to fexecve");
							}
							unreachable!()
						})
					}
					unistd::ForkResult::Parent { child, .. } => child,
				};
				unistd::close(process_listener).unwrap();
				let x = pending.write().unwrap().insert(process_id, child);
				assert!(x.is_none());
				trace.fabric(FabricOutputEvent::Init {
					pid: process_id,
					system_pid: nix::libc::pid_t::from(child).try_into().unwrap(),
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
				let _ = scope.spawn(move |_scope| {
					loop {
						match wait::waitpid(child, None) {
							Err(nix::Error::Sys(nix::errno::Errno::EINTR)) => (),
							Ok(wait::WaitStatus::Exited(pid, code)) if code == 0 => {
								assert_eq!(pid, child);
								break;
							}
							Ok(wait::WaitStatus::Signaled(pid, signal, _))
								if signal == signal::Signal::SIGKILL =>
							{
								assert_eq!(pid, child);
								break;
							}
							wait_status => panic!("{:?}", wait_status),
						}
					}
					let x = pending.write().unwrap().remove(&process_id).unwrap();
					assert_eq!(x, child);
					trace.fabric(FabricOutputEvent::Exit {
						pid: process_id,
						system_pid: nix::libc::pid_t::from(child).try_into().unwrap(),
					});
					let _unchecked_error = bincode::serialize_into(
						*stream_write.lock().unwrap(),
						&Either::Right::<Pid, Pid>(process_id),
					)
					.map_err(map_bincode_err);
				});
			}
			for (&_job, &pid) in pending.read().unwrap().iter() {
				let _unchecked_error = signal::kill(pid, signal::Signal::SIGKILL);
			}
		})
		.unwrap();
		assert_eq!(pending_inner.len(), 0);
	}
}
