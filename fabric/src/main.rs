//! This is the fabric binary that, when run one or more times, constitutes a fabric cluster.
//!
//! fabric
//! Run a fabric worker, optionally as master, and optionally start a bridge running
//!
//! USAGE:
//!     fabric master (<addr> <mem> <cpu> [<bridge> <addr>]...)...
//!     fabric <addr>
//! 
//! OPTIONS:
//!     -h --help          Show this screen.
//!     -V --version       Show version.
//!
//! A fabric cluster comprises one or more workers, where one is declared master.
//! 
//! The arguments to the master worker are the address and resources of each worker,
//! including itself. The arguments for a worker can include a binary to spawn
//! immediately and an address to reserve for it. This is intended to be used to
//! spawn the bridge, which works with the deploy command and library to handle
//! transparent capture and forwarding of output and debug information.
//! 
//! The first set of arguments to the master worker is for itself – as such the
//! address is bound to not connected to.
//! 
//! The argument to non-master workers is the address to bind to.
//! 
//! For example, respective invocations on each of a cluster of 3 servers with
//! 512GiB memory and 36 logical cores apiece might be:
//!     fabric master 10.0.0.1:9999 400GiB 34 bridge 10.0.0.1:8888 \
//!                   10.0.0.2:9999 400GiB 34 \
//!                   10.0.0.3:9999 400GiB 34
//!     fabric 10.0.0.2:9999
//!     fabric 10.0.0.3:9999
//! 
//! Deploying to this cluster might then be:
//!     deploy 10.0.0.1:8888 ./binary
//! or, for a Rust crate:
//!     cargo deploy 10.0.0.1:8888

#![doc(html_root_url = "https://docs.rs/fabric/0.1.0")]
#![feature(global_allocator,allocator_api)]
extern crate nix;
extern crate bincode;
extern crate crossbeam;
extern crate either;
extern crate serde;
#[macro_use] extern crate serde_derive;
extern crate serde_json;
extern crate deploy_common;

mod scheduler;

use std::{ffi,net,fs,io,env,sync,mem,iter,thread,process};
use std::sync::mpsc;
use std::collections::HashMap;
use std::io::{Write,Read};
use std::os::unix;
use std::os::unix::io::{FromRawFd,IntoRawFd,AsRawFd};
use std::path::PathBuf;
use either::Either;

use deploy_common::{copy,BufferedStream,map_bincode_err,is_valgrind,valgrind_start_fd,move_fds,parse_binary_size,Resources};

#[global_allocator]
static GLOBAL: std::alloc::System = std::alloc::System;

const DESCRIPTION: &'static str = r"
fabric
Run a fabric worker, optionally as master, and optionally start a bridge running
";
const USAGE: &'static str = r"
USAGE:
    fabric master (<addr> <mem> <cpu> [<bridge> <addr>]...)...
    fabric <addr>

OPTIONS:
    -h --help          Show this screen.
    -V --version       Show version.
";
const HELP: &'static str = r"
A fabric cluster comprises one or more workers, where one is declared master.

The arguments to the master worker are the address and resources of each worker,
including itself. The arguments for a worker can include a binary to spawn
immediately and an address to reserve for it. This is intended to be used to
spawn the bridge, which works with the deploy command and library to handle
transparent capture and forwarding of output and debug information.

The first set of arguments to the master worker is for itself – as such the
address is bound to not connected to.

The argument to non-master workers is the address to bind to.

For example, respective invocations on each of a cluster of 3 servers with
512GiB memory and 36 logical cores apiece might be:
    fabric master 10.0.0.1:9999 400GiB 34 bridge 10.0.0.1:8888 \
                  10.0.0.2:9999 400GiB 34 \
                  10.0.0.3:9999 400GiB 34
    fabric 10.0.0.2:9999
    fabric 10.0.0.3:9999

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
	fn from_argv() -> Arg {
		let mut args = env::args().peekable(); args.next().unwrap();
		match args.next().as_ref().map(|x|&**x) {
			Some("master") => {
				let mut nodes = Vec::new();
				loop {
					match (args.next().map(|x|x.parse()),args.next().map(|x|parse_binary_size(&x)),args.next().map(|x|x.parse())) {
						(None,_,_) if nodes.len() > 0 => break,
						(Some(Ok(addr)),Some(Ok(mem)),Some(Ok(cpu))) => {
							let mut run = Vec::new();
							loop {
								match args.peek().map(|x|x.parse::<net::SocketAddr>()) {
									Some(Err(_binary)) => {
										match (args.next().unwrap(),args.next().map(|x|x.parse())) {
											(binary,Some(Ok(addr))) => run.push(Run{binary:PathBuf::from(binary),addr}),
											_ => {println!("Invalid bridge, expecting bridge: <bridge> <addr> or worker options: <addr> <mem> <cpu>\n{}", USAGE); process::exit(1)},
										}
									}
									None | Some(Ok(_)) => break,
								}
							}
							nodes.push(Node{addr,mem,cpu,run});
						}
						_ => {println!("Invalid worker options, expecting <addr> <mem> <cpu>, like 127.0.0.1:9999 400GiB 34\n{}", USAGE); process::exit(1)},
					}
				}
				Arg::Master(nodes)
			},
			None | Some("-h") | Some("--help") => {println!("{}{}{}", DESCRIPTION, USAGE, HELP); process::exit(0)},
			Some("-V") | Some("--version") => {println!("fabric {}", env!("CARGO_PKG_VERSION")); process::exit(0)},
			Some(x) if x.parse::<net::SocketAddr>().is_ok() => {
				Arg::Worker(x.parse::<net::SocketAddr>().unwrap())
			},
			Some(x) => {println!("Invalid option \"{}\", expecting either \"master\" or an address, like 127.0.0.1:9999\n{}", x, USAGE); process::exit(1)},
		}
	}
}

const LISTENER_FD: std::os::unix::io::RawFd = 3;
const ARG_FD: std::os::unix::io::RawFd = 4;
const BOUND_FD_START: std::os::unix::io::RawFd = 5;

fn parse_request<R: Read>(mut stream: &mut R) -> Result<(Resources,Vec<net::SocketAddr>,fs::File,Vec<ffi::OsString>,Vec<(ffi::OsString,ffi::OsString)>,fs::File),io::Error> {
	let resources = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let ports: Vec<net::SocketAddr> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let args: Vec<ffi::OsString> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let vars: Vec<(ffi::OsString,ffi::OsString)> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let len: u64 = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let mut elf = unsafe{fs::File::from_raw_fd(nix::sys::memfd::memfd_create(&ffi::CString::new(unix::ffi::OsStringExt::into_vec(args[0].clone())).unwrap(), nix::sys::memfd::MemFdCreateFlag::MFD_CLOEXEC).expect("Failed to memfd_create"))};
	assert!(nix::fcntl::FdFlag::from_bits(nix::fcntl::fcntl(elf.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFD).unwrap()).unwrap().contains(nix::fcntl::FdFlag::FD_CLOEXEC));
	nix::unistd::ftruncate(elf.as_raw_fd(), len as i64).unwrap();
	copy(stream, &mut elf, len as usize)?;
	let x = nix::unistd::lseek64(elf.as_raw_fd(), 0, nix::unistd::Whence::SeekSet).unwrap(); assert_eq!(x, 0);
	let spawn_arg: Vec<u8> = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let mut arg = unsafe{fs::File::from_raw_fd(nix::sys::memfd::memfd_create(&ffi::CString::new(unix::ffi::OsStringExt::into_vec(args[0].clone())).unwrap(), nix::sys::memfd::MemFdCreateFlag::empty()).expect("Failed to memfd_create"))};
	nix::unistd::ftruncate(arg.as_raw_fd(), spawn_arg.len() as i64).unwrap();
	arg.write_all(&spawn_arg).unwrap();
	let x = nix::unistd::lseek64(arg.as_raw_fd(), 0, nix::unistd::Whence::SeekSet).unwrap(); assert_eq!(x, 0);
	Ok((resources, ports, elf, args, vars, arg))
}

fn main() {
	let arg = Arg::from_argv();
	let (listen,listener) = match arg {
		Arg::Master(mut nodes) => {
			let fabric = net::TcpListener::bind("127.0.0.1:0").unwrap();
			let scheduler_addr = nodes[0].addr;
			nodes[0].addr = fabric.local_addr().unwrap();
			thread::Builder::new().name(String::from("scheduler")).spawn(move||{
				scheduler::run(scheduler_addr, nodes.into_iter().map(|Node{addr,mem,cpu,run}|(addr,(mem,cpu,run.into_iter().map(|Run{binary,addr}|(binary,vec![addr])).collect()))).collect::<HashMap<_,_>>()); // TODO: error on clash
			}).unwrap();
			(scheduler_addr.ip(),fabric)
		}
		Arg::Worker(listen) => {
			(listen.ip(),net::TcpListener::bind(&listen).unwrap())
		}
	};
	let mut count = 0;
	for stream in listener.incoming() { let stream = stream.unwrap();
		println!("accepted");
		let mut pending_inner = HashMap::new();
		{
			let mut pending = &sync::RwLock::new(&mut pending_inner);
			let (sender, receiver) = mpsc::sync_channel::<(nix::unistd::Pid,Either<u16,u16>)>(0);
			let (mut stream_read,mut stream_write) = (BufferedStream::new(&stream),&stream);
			crossbeam::scope(|scope|{
				scope.spawn(move||{
					let receiver = receiver;
					for (pid,done) in receiver.iter() {
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
						if let Err(_) = bincode::serialize_into(&mut stream_write, &done).map_err(map_bincode_err) {
							break;
						}
					}
					for _done in receiver.iter() { }
				});
				while let Ok((resources, ports, elf, args, vars, arg)) = parse_request(&mut stream_read) {
					let process_listener = nix::sys::socket::socket(nix::sys::socket::AddressFamily::Inet, nix::sys::socket::SockType::Stream, nix::sys::socket::SockFlag::SOCK_NONBLOCK, nix::sys::socket::SockProtocol::Tcp).unwrap();
					nix::sys::socket::setsockopt(process_listener, nix::sys::socket::sockopt::ReuseAddr, &true).unwrap();
					nix::sys::socket::bind(process_listener, &nix::sys::socket::SockAddr::Inet(nix::sys::socket::InetAddr::from_std(&net::SocketAddr::new(listen, 0)))).unwrap();
					nix::sys::socket::setsockopt(process_listener, nix::sys::socket::sockopt::ReusePort, &true).unwrap();
					let process_id = if let nix::sys::socket::SockAddr::Inet(inet) = nix::sys::socket::getsockname(process_listener).unwrap() {inet.to_std()} else {panic!()}.port();
					let child = match nix::unistd::fork().expect("Fork failed") {
						nix::unistd::ForkResult::Child => {
							// println!("{:?}", args[0]);
							let err = unsafe{nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL)}; assert_eq!(err, 0);
							nix::unistd::setpgid(nix::unistd::Pid::from_raw(0),nix::unistd::Pid::from_raw(0)).unwrap();
							let elf = elf.into_raw_fd();
							let mut elf_desired_fd = BOUND_FD_START+ports.len() as std::os::unix::io::RawFd;
							let arg = arg.into_raw_fd();
							move_fds(&mut [(arg, ARG_FD),(process_listener,LISTENER_FD),(elf,elf_desired_fd)]);
							for (i,port) in ports.into_iter().enumerate() {
								let socket: std::os::unix::io::RawFd = BOUND_FD_START+i as std::os::unix::io::RawFd;
								let fd = nix::sys::socket::socket(nix::sys::socket::AddressFamily::Inet, nix::sys::socket::SockType::Stream, nix::sys::socket::SockFlag::empty(), nix::sys::socket::SockProtocol::Tcp).unwrap();
								if fd != socket {
									let new_fd = nix::unistd::dup2(fd, socket).unwrap(); assert_eq!(new_fd, socket);
									nix::unistd::close(fd).unwrap();
								}
								nix::sys::socket::setsockopt(socket, nix::sys::socket::sockopt::ReuseAddr, &true).unwrap();
								nix::sys::socket::bind(socket, &nix::sys::socket::SockAddr::Inet(nix::sys::socket::InetAddr::from_std(&port))).unwrap();
							}
							let vars = iter::once((ffi::CString::new("DEPLOY").unwrap(),ffi::CString::new("fabric").unwrap())).chain(iter::once((ffi::CString::new("DEPLOY_RESOURCES").unwrap(),ffi::CString::new(serde_json::to_string(&resources).unwrap()).unwrap()))).chain(vars.into_iter().map(|(x,y)|(ffi::CString::new(unix::ffi::OsStringExt::into_vec(x)).unwrap(),ffi::CString::new(unix::ffi::OsStringExt::into_vec(y)).unwrap()))).map(|(key,value)|ffi::CString::new(format!("{}={}",key.to_str().unwrap(),value.to_str().unwrap())).unwrap()).collect::<Vec<_>>();
							if false {
								nix::unistd::execve(&ffi::CString::new(unix::ffi::OsStringExt::into_vec(args[0].clone())).unwrap(), &args.into_iter().map(|x|ffi::CString::new(unix::ffi::OsStringExt::into_vec(x)).unwrap()).collect::<Vec<_>>(), &vars).expect("Failed to fexecve ELF");
							} else {
								if is_valgrind() {
									let elf_desired_fd_ = valgrind_start_fd()-1; assert!(elf_desired_fd_ > elf_desired_fd);
									nix::unistd::dup2(elf_desired_fd, elf_desired_fd_).unwrap();
									nix::unistd::close(elf_desired_fd).unwrap();
									elf_desired_fd = elf_desired_fd_;
								}
								nix::unistd::fexecve(elf_desired_fd, &args.into_iter().map(|x|ffi::CString::new(unix::ffi::OsStringExt::into_vec(x)).unwrap()).collect::<Vec<_>>(), &vars).expect("Failed to fexecve ELF");
							}
							unreachable!();
						}
						nix::unistd::ForkResult::Parent{child, ..} => child,
					};
					nix::unistd::close(process_listener).unwrap();
					let x = pending.write().unwrap().insert(process_id, child); assert!(x.is_none());
					sender.send((child,Either::Left(process_id))).unwrap();
					let sender = sender.clone();
					scope.spawn(move||{
						match nix::sys::wait::waitpid(child, None).unwrap() {
							nix::sys::wait::WaitStatus::Exited(pid, code) if code == 0 => assert_eq!(pid, child),
							nix::sys::wait::WaitStatus::Signaled(pid, signal, _) if signal == nix::sys::signal::Signal::SIGKILL => assert_eq!(pid, child),
							wait_status => panic!("{:?}", wait_status),
						}
						pending.write().unwrap().remove(&process_id).unwrap();
						sender.send((child,Either::Right(process_id))).unwrap();
					});
				}
				for (&_job,&pid) in pending.read().unwrap().iter() {
					let _unchecked_error = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
				}
				mem::drop(sender); // otherwise the done-forwarding thread never ends
			});
		}
		assert_eq!(pending_inner.len(), 0);
	}
}
