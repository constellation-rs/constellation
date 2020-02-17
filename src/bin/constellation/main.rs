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
	clippy::too_many_lines,
	clippy::erasing_op
)]

mod args;
mod bridge;
#[cfg(feature = "kubernetes")]
mod kube;
mod mesh;

use either::Either;
#[cfg(unix)]
use nix::{fcntl, sys::signal, sys::socket, unistd};
use palaver::{
	file::{execve, fexecve, move_fd, move_fds, pipe, OFlag}, process::ChildHandle, socket::{socket, SockFlag}, valgrind
};
use std::{
	collections::HashMap, convert::{TryFrom, TryInto}, env, io, io::Seek, net::{IpAddr, SocketAddr}, process, sync, sync::Arc
};
#[cfg(unix)]
use std::{
	ffi::{CStr, CString, OsString}, fs::File, os::unix::io::FromRawFd, os::unix::{ffi::OsStringExt, io::IntoRawFd}
};

#[cfg(feature = "kubernetes")]
use self::kube::kube_master;
use constellation_internal::{
	abort_on_unwind_1, enable_backtrace, file_from_reader, forbid_alloc, map_bincode_err, msg::{bincode_deserialize_from, FabricRequest}, BufferedStream, Cpu, FabricOutputEvent, Fd, Format, Key, Mem, Pid, PidInternal, Resources, Trace
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
	Master(SocketAddr, Key, Vec<Node>),
	Worker(SocketAddr, Key),
	Mesh(MeshRole),
	Bridge,
}
#[derive(PartialEq, Debug)]
enum MeshRole {
	Master(SocketAddr, Key, Vec<Node>),
	Worker(SocketAddr, Key),
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
	enable_backtrace();
	let args = Args::from_args(env::args().skip(1)).unwrap_or_else(|(message, success)| {
		println!("{}", message);
		process::exit(if success { 0 } else { 1 })
	});
	if let Role::Bridge = args.role {
		bridge::main();
	}
	if let Role::Mesh(role) = args.role {
		mesh::main(role);
	}
	let trace = &Trace::new(io::stdout(), args.format, args.verbose);
	let bind_ip = match &args.role {
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
		Role::Master(bind, _key, _nodes) => bind.ip(),
		Role::Worker(bind, _key) => bind.ip(),
		Role::Bridge | Role::Mesh(_) => unreachable!(),
	};

	loop {
		let mut pending_inner = HashMap::new();
		let pending = &sync::RwLock::new(&mut pending_inner);
		crossbeam::scope(|scope| {
			let (read, write) = {
				let (cluster_ip, args, key) = match &args.role {
					Role::Master(bind, key, nodes) => {
						let mut args = vec![
							OsString::from(env::current_exe().unwrap()),
							OsString::from("mesh-master"),
							OsString::from(bind.to_string()),
							OsString::from(key.to_string()),
						];
						for node in nodes {
							args.extend(
								vec![
									node.fabric.to_string(),
									node.bridge
										.map_or(String::from("-"), |bridge| bridge.to_string()),
									node.mem.to_string(),
									node.cpu.to_string(),
								]
								.into_iter()
								.map(OsString::from),
							);
						}
						let cluster_ip = nodes[0].fabric.ip();
						(cluster_ip, args, *key)
					}
					Role::Worker(bind, key) => {
						let args = vec![
							OsString::from(env::current_exe().unwrap()),
							OsString::from("mesh-worker"),
							OsString::from(bind.to_string()),
							OsString::from(key.to_string()),
						];
						let cluster_ip = std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
						(cluster_ip, args, *key)
					}
					_ => unreachable!(),
				};
				let (host_read, mesh_write) = pipe(OFlag::empty()).unwrap();
				let (mesh_read, host_write) = pipe(OFlag::empty()).unwrap();
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
				let (pid, child) = spawn(
					bind_ip,
					cluster_ip,
					request,
					Some(key),
					Some([mesh_read, mesh_write]),
				);
				unistd::close(mesh_read).unwrap();
				unistd::close(mesh_write).unwrap();
				let child = Arc::new(child);
				let x = pending.write().unwrap().insert(pid, child.clone());
				assert!(x.is_none());
				trace.fabric(FabricOutputEvent::Init {
					pid,
					system_pid: nix::libc::pid_t::from(child.pid).try_into().unwrap(),
				});
				let _ = scope.spawn(abort_on_unwind_1(move |_scope| {
					match child.wait() {
						Ok(palaver::process::WaitStatus::Exited(0)) => (),
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
				(host_read, host_write)
			};

			let read = unsafe { File::from_raw_fd(read) };
			let write = unsafe { File::from_raw_fd(write) };

			let (mut stream_read, stream_write) =
				(BufferedStream::new(read), &sync::Mutex::new(write));
			crossbeam::scope(|scope| {
				let cluster_ip = bincode::deserialize_from::<_, IpAddr>(&mut stream_read);
				if let Ok(cluster_ip) = cluster_ip {
					while let Ok(request) =
						bincode_deserialize_from(&mut stream_read).map_err(map_bincode_err)
					{
						let request: FabricRequest<File, File> = request;
						let (pid, child) = spawn(bind_ip, cluster_ip, request, None, None);
						let child = Arc::new(child);
						let x = pending.write().unwrap().insert(pid, child.clone());
						assert!(x.is_none());
						trace.fabric(FabricOutputEvent::Init {
							pid,
							system_pid: nix::libc::pid_t::from(child.pid).try_into().unwrap(),
						});
						if bincode::serialize_into(
							&mut *stream_write.lock().unwrap(),
							&Either::Left::<Pid, Pid>(pid),
						)
						.map_err(map_bincode_err)
						.is_err()
						{
							break;
						}
						let _ = scope.spawn(abort_on_unwind_1(move |_scope| {
							match child.wait() {
								Ok(palaver::process::WaitStatus::Exited(0)) => (),
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
							let _unchecked_error = bincode::serialize_into(
								&mut *stream_write.lock().unwrap(),
								&Either::Right::<Pid, Pid>(pid),
							)
							.map_err(map_bincode_err);
						}));
					}
				}
			})
			.unwrap();

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

fn spawn(
	bind_ip: IpAddr, cluster_ip: IpAddr, request: FabricRequest<File, File>, key: Option<Key>,
	host: Option<[Fd; 2]>,
) -> (Pid, ChildHandle) {
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
		&socket::SockAddr::Inet(socket::InetAddr::from_std(&SocketAddr::new(bind_ip, 0))),
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
	let pid = Pid::new(cluster_ip, port, key);
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
	let binary_desired_fd = if cfg!(feature = "distribute_binaries") {
		Some(
			BOUND_FD_START
				+ host.map_or(0, |host| host.len().try_into().unwrap())
				+ Fd::try_from(request.bind.len()).unwrap(),
		)
	} else {
		None
	};
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

				let mut fds = arrayvec::ArrayVec::<[_; 5]>::new();
				fds.push((process_listener, LISTENER_FD));
				fds.push((arg.into_raw_fd(), ARG_FD));
				if let Some(host) = host {
					for (i, &host) in host.iter().enumerate() {
						fds.push((host, BOUND_FD_START + Fd::try_from(i).unwrap()));
					}
				}
				#[cfg(feature = "distribute_binaries")]
				fds.push((binary.into_raw_fd(), binary_desired_fd.unwrap()));
				move_fds(&mut fds, Some(fcntl::FdFlag::empty()), true);
				for fd in
					BOUND_FD_START + host.map_or(0, |host| host.len().try_into().unwrap())..1024
				{
					if Some(fd) == binary_desired_fd {
						continue;
					}
					let _ = unistd::close(fd);
				}
				for (i, addr) in bind.iter().enumerate() {
					let socket: Fd = BOUND_FD_START
						+ host.map_or(0, |host| host.len().try_into().unwrap())
						+ Fd::try_from(i).unwrap();
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
				for fd in 0..BOUND_FD_START
					+ host.map_or(0, |host| host.len().try_into().unwrap())
					+ i32::try_from(bind.len()).unwrap()
					+ cfg!(feature = "distribute_binaries") as i32
				{
					let flags = fcntl::FdFlag::from_bits(
						fcntl::fcntl(fd, fcntl::FcntlArg::F_GETFD).unwrap(),
					)
					.unwrap();
					assert!(!flags.contains(fcntl::FdFlag::FD_CLOEXEC));
				}
				if cfg!(feature = "distribute_binaries") {
					let mut binary_desired_fd = binary_desired_fd.unwrap();
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
