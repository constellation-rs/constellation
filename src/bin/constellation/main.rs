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

mod master;

use either::Either;
#[cfg(unix)]
use nix::{fcntl, sys::signal, sys::socket, sys::wait, unistd};
use palaver::{
	file::{copy, copy_fd, fexecve, memfd_create, move_fds, seal_fd}, socket::{socket, SockFlag}, valgrind
};
use std::{
	collections::HashMap, convert::{TryFrom, TryInto}, env, ffi::OsString, fs, io::{self, Read, Write}, iter, net, path::PathBuf, process, sync::{self, mpsc}
};
#[cfg(unix)]
use std::{
	ffi::CString, os::unix::{
		ffi::OsStringExt, io::{AsRawFd, FromRawFd, IntoRawFd}
	}
};

use constellation_internal::{map_bincode_err, parse_binary_size, BufferedStream, Resources};

#[cfg(unix)]
type Fd = std::os::unix::io::RawFd;
#[cfg(windows)]
type Fd = std::os::windows::io::RawHandle;

const DESCRIPTION: &str = r"
constellation
Run a constellation node, optionally as master, and optionally start a bridge running
";
const USAGE: &str = r"
USAGE:
    constellation master (<addr> <mem> <cpu> [<bridge> <addr>]...)...
    constellation <addr>

OPTIONS:
    -h --help          Show this screen.
    -V --version       Show version.
";
const HELP: &str = r"
A constellation cluster comprises one or more nodes, where one is declared master.

The arguments to the master node are the address and resources of each node,
including itself. The arguments for a node can include a binary to spawn
immediately and an address to reserve for it. This is intended to be used to
spawn the bridge, which works with the deploy command and library to handle
transparent capture and forwarding of output and debug information.

The first set of arguments to the master node is for itself – as such the
address is bound to not connected to.

The argument to non-master nodes is the address to bind to.

For example, respective invocations on each of a cluster of 3 servers with
512GiB memory and 36 logical cores apiece might be:
    constellation master 10.0.0.1:9999 400GiB 34 bridge 10.0.0.1:8888 \
                  10.0.0.2:9999 400GiB 34 \
                  10.0.0.3:9999 400GiB 34
    constellation 10.0.0.2:9999
    constellation 10.0.0.3:9999

Deploying to this cluster might then be:
    deploy 10.0.0.1:8888 ./binary
or, for a Rust crate:
    cargo deploy 10.0.0.1:8888
";

#[derive(Debug)]
enum Arg {
	Master(Vec<Node>),
	Worker(net::SocketAddr),
}
#[derive(Debug)]
struct Node {
	addr: net::SocketAddr,
	mem: u64,
	cpu: f32,
	run: Vec<Run>,
}
#[derive(Debug)]
struct Run {
	binary: PathBuf,
	addr: net::SocketAddr,
}

impl Arg {
	fn from_argv() -> Self {
		let mut args = env::args().peekable();
		let _ = args.next().unwrap();
		match args.next().as_ref().map(|x| &**x) {
			Some("master") => {
				let mut nodes = Vec::new();
				loop {
					match (
						args.next().map(|x| x.parse()),
						args.next().map(|x| parse_binary_size(&x)),
						args.next().map(|x| x.parse()),
					) {
						(None, _, _) if !nodes.is_empty() => break,
						(Some(Ok(addr)), Some(Ok(mem)), Some(Ok(cpu))) => {
							let mut run = Vec::new();
							while let Some(Err(_binary)) =
								args.peek().map(|x| x.parse::<net::SocketAddr>())
							{
								if let (binary, Some(Ok(addr))) =
									(args.next().unwrap(), args.next().map(|x| x.parse()))
								{
									run.push(Run {
										binary: PathBuf::from(binary),
										addr,
									});
								} else {
									println!("Invalid bridge, expecting bridge: <bridge> <addr> or node options: <addr> <mem> <cpu>\n{}", USAGE);
									process::exit(1)
								}
							}
							nodes.push(Node {
								addr,
								mem,
								cpu,
								run,
							});
						}
						_ => {
							println!("Invalid node options, expecting <addr> <mem> <cpu>, like 127.0.0.1:9999 400GiB 34\n{}", USAGE);
							process::exit(1)
						}
					}
				}
				Arg::Master(nodes)
			}
			None | Some("-h") | Some("--help") => {
				println!("{}{}{}", DESCRIPTION, USAGE, HELP);
				process::exit(0)
			}
			Some("-V") | Some("--version") => {
				println!("constellation {}", env!("CARGO_PKG_VERSION"));
				process::exit(0)
			}
			Some(x) if x.parse::<net::SocketAddr>().is_ok() => {
				Arg::Worker(x.parse::<net::SocketAddr>().unwrap())
			}
			Some(x) => {
				println!("Invalid option \"{}\", expecting either \"master\" or an address, like 127.0.0.1:9999\n{}", x, USAGE);
				process::exit(1)
			}
		}
	}
}

const LISTENER_FD: Fd = 3;
const ARG_FD: Fd = 4;
const BOUND_FD_START: Fd = 5;

fn parse_request<R: Read>(
	mut stream: &mut R,
) -> Result<
	(
		Resources,
		Vec<net::SocketAddr>,
		fs::File,
		Vec<OsString>,
		Vec<(OsString, OsString)>,
		fs::File,
	),
	io::Error,
> {
	let resources = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let ports: Vec<net::SocketAddr> =
		bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let args: Vec<OsString> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let vars: Vec<(OsString, OsString)> =
		bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let len: u64 = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let mut binary = unsafe {
		fs::File::from_raw_fd(
			memfd_create(
				&CString::new(OsStringExt::into_vec(args[0].clone())).unwrap(),
				true,
			)
			.expect("Failed to memfd_create"),
		)
	};
	assert!(fcntl::FdFlag::from_bits(
		fcntl::fcntl(binary.as_raw_fd(), fcntl::FcntlArg::F_GETFD).unwrap()
	)
	.unwrap()
	.contains(fcntl::FdFlag::FD_CLOEXEC));
	unistd::ftruncate(binary.as_raw_fd(), len.try_into().unwrap()).unwrap();
	copy(stream, &mut binary, len)?;
	let x = unistd::lseek(binary.as_raw_fd(), 0, unistd::Whence::SeekSet).unwrap();
	assert_eq!(x, 0);
	seal_fd(binary.as_raw_fd());
	let spawn_arg: Vec<u8> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let mut arg = unsafe {
		fs::File::from_raw_fd(
			memfd_create(
				&CString::new(OsStringExt::into_vec(args[0].clone())).unwrap(),
				false,
			)
			.expect("Failed to memfd_create"),
		)
	};
	unistd::ftruncate(arg.as_raw_fd(), spawn_arg.len().try_into().unwrap()).unwrap();
	arg.write_all(&spawn_arg).unwrap();
	let x = unistd::lseek(arg.as_raw_fd(), 0, unistd::Whence::SeekSet).unwrap();
	assert_eq!(x, 0);
	Ok((resources, ports, binary, args, vars, arg))
}

fn main() {
	let arg = Arg::from_argv();
	let (listen, listener) = match arg {
		Arg::Master(mut nodes) => {
			let fabric = net::TcpListener::bind("127.0.0.1:0").unwrap();
			let scheduler_addr = nodes[0].addr;
			nodes[0].addr = fabric.local_addr().unwrap();
			let _ = std::thread::Builder::new()
				.name(String::from("master"))
				.spawn(move || {
					master::run(
						scheduler_addr,
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
			(scheduler_addr.ip(), fabric)
		}
		Arg::Worker(listen) => (listen.ip(), net::TcpListener::bind(&listen).unwrap()),
	};
	let mut count = 0;
	for stream in listener.incoming() {
		let stream = stream.unwrap();
		println!("accepted");
		let mut pending_inner = HashMap::new();
		{
			let pending = &sync::RwLock::new(&mut pending_inner);
			let (sender, receiver) = mpsc::sync_channel::<(unistd::Pid, Either<u16, u16>)>(0);
			let (mut stream_read, mut stream_write) = (BufferedStream::new(&stream), &stream);
			crossbeam::scope(|scope| {
				let _ = scope.spawn(move || {
					let receiver = receiver;
					for (pid, done) in receiver.iter() {
						match done {
							Either::Left(init) => {
								count += 1;
								println!("FABRIC: init({}) {}:{}", count, pid, init);
							}
							Either::Right(exit) => {
								count -= 1;
								println!("FABRIC: exit({}) {}:{}", count, pid, exit);
							}
						}
						if bincode::serialize_into(&mut stream_write, &done)
							.map_err(map_bincode_err)
							.is_err()
						{
							break;
						}
					}
					for _done in receiver.iter() {}
				});
				while let Ok((resources, ports, binary, args, vars, arg)) =
					parse_request(&mut stream_read)
				{
					let process_listener = socket(
						socket::AddressFamily::Inet,
						socket::SockType::Stream,
						SockFlag::SOCK_NONBLOCK,
						socket::SockProtocol::Tcp,
					)
					.unwrap();
					socket::setsockopt(process_listener, socket::sockopt::ReuseAddr, &true)
						.unwrap();
					socket::bind(
						process_listener,
						&socket::SockAddr::Inet(socket::InetAddr::from_std(&net::SocketAddr::new(
							listen, 0,
						))),
					)
					.unwrap();
					socket::setsockopt(process_listener, socket::sockopt::ReusePort, &true)
						.unwrap();
					let process_id = if let socket::SockAddr::Inet(inet) =
						socket::getsockname(process_listener).unwrap()
					{
						inet.to_std()
					} else {
						panic!()
					}
					.port();
					let child = match unistd::fork().expect("Fork failed") {
						unistd::ForkResult::Child => {
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
							for (i, port) in ports.into_iter().enumerate() {
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
							if false {
								unistd::execve(
									&CString::new(OsStringExt::into_vec(args[0].clone())).unwrap(),
									&args
										.into_iter()
										.map(|x| CString::new(OsStringExt::into_vec(x)).unwrap())
										.collect::<Vec<_>>(),
									&vars,
								)
								.expect("Failed to fexecve");
							} else {
								if valgrind::is() {
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
								fexecve(
									binary_desired_fd,
									&args
										.into_iter()
										.map(|x| CString::new(OsStringExt::into_vec(x)).unwrap())
										.collect::<Vec<_>>(),
									&vars,
								)
								.expect("Failed to fexecve");
							}
							unreachable!();
						}
						unistd::ForkResult::Parent { child, .. } => child,
					};
					unistd::close(process_listener).unwrap();
					let x = pending.write().unwrap().insert(process_id, child);
					assert!(x.is_none());
					sender.send((child, Either::Left(process_id))).unwrap();
					let sender = sender.clone();
					let _ = scope.spawn(move || {
						match wait::waitpid(child, None).unwrap() {
							wait::WaitStatus::Exited(pid, code) if code == 0 => {
								assert_eq!(pid, child)
							}
							wait::WaitStatus::Signaled(pid, signal, _)
								if signal == signal::Signal::SIGKILL =>
							{
								assert_eq!(pid, child)
							}
							wait_status => panic!("{:?}", wait_status),
						}
						let _ = pending.write().unwrap().remove(&process_id).unwrap();
						sender.send((child, Either::Right(process_id))).unwrap();
					});
				}
				for (&_job, &pid) in pending.read().unwrap().iter() {
					let _unchecked_error = signal::kill(pid, signal::Signal::SIGKILL);
				}
				drop(sender); // otherwise the done-forwarding thread never ends
			});
		}
		assert_eq!(pending_inner.len(), 0);
	}
}
