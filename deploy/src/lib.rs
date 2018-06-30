//! Distributed programming primitives.
//!
//! This library provides a runtime to aide in the writing and debugging of distributed programs.
//!
//! The two key ideas are:
//!
//!  * **Spawning new processes:** The [spawn()](spawn) function can be used to spawn a new process running a particular function.
//!  * **Channels:** [Sender]s and [Receiver]s can be used for synchronous or asynchronous inter-process communication.
//!
//! The only requirement to use is that [init()](init) must be called immediately inside your application's `main()` function.

#![doc(html_root_url = "https://docs.rs/deploy/0.1.2")]
#![feature(global_allocator, allocator_api, read_initializer, linkage, core_intrinsics, nll)]
#![deny(missing_docs, warnings, deprecated)]
#![allow(dead_code)]

extern crate ansi_term;
extern crate atty;
extern crate bincode;
extern crate deploy_common;
extern crate either;
extern crate fringe;
extern crate futures;
extern crate itertools;
extern crate nix;
extern crate rand;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate lazy_static;

pub use channel::{ChannelError, Selectable};
use deploy_common::{
	copy_sendfile, dup2, is_valgrind, map_bincode_err, memfd_create, spawn as thread_spawn, valgrind_start_fd, BufferedStream, Deploy, DeployOutputEvent, Envs, FdIter, Format, Formatter, PidInternal, ProcessInputEvent, ProcessOutputEvent, StyleSupport
};
pub use deploy_common::{Pid, Resources, DEPLOY_RESOURCES_DEFAULT};
use either::Either;
use std::{
	alloc, ffi, fmt, fs, intrinsics,
	io::{self, Read, Write},
	iter, marker, mem, net, ops,
	os::{
		self,
		unix::io::{AsRawFd, FromRawFd, IntoRawFd},
	},
	path, process, str,
	sync::{self, mpsc},
	thread,
};

// macro_rules! log { // prints to STDOUT
// 	($($arg:tt)*) => (let mut file = ::std::io::BufWriter::with_capacity(4096/*PIPE_BUF*/, unsafe{<::std::fs::File as ::std::os::unix::io::FromRawFd>::from_raw_fd(2)}); <::std::io::BufWriter<::std::fs::File> as ::std::io::Write>::write_fmt(&mut file, format_args!($($arg)*)).unwrap(); <::std::fs::File as ::std::os::unix::io::IntoRawFd>::into_raw_fd(file.into_inner().unwrap()););
// }
macro_rules! log {
	($($arg:tt)*) => (format_args!($($arg)*));
}
macro_rules! logln {
	() => (log!("\n"));
	($fmt:expr) => (log!(concat!($fmt, "\n")));
	($fmt:expr, $($arg:tt)*) => (log!(concat!($fmt, "\n"), $($arg)*));
}
#[allow(unused_macros)]
macro_rules! print {
	($($arg:tt)*) => {
		compile_error!("Cannot use print!()")
	};
}
#[allow(unused_macros)]
macro_rules! eprint {
	($($arg:tt)*) => {
		compile_error!("Cannot use eprint!()")
	};
}

// mod alloc;
mod channel;

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#[global_allocator]
static GLOBAL: alloc::System = alloc::System;
// static GLOBAL: alloc::HAlloc<alloc::System> = alloc::HAlloc::new(alloc::System);

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

const LISTENER_FD: os::unix::io::RawFd = 3; // from fabric
const ARG_FD: os::unix::io::RawFd = 4; // from fabric
const SCHEDULER_FD: os::unix::io::RawFd = 4;
const MONITOR_FD: os::unix::io::RawFd = 5;

#[derive(Clone, Deserialize, Debug)]
struct SchedulerArg {
	scheduler: net::SocketAddr,
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

lazy_static! {
	static ref BRIDGE: sync::RwLock<Option<Pid>> = sync::RwLock::new(None);
	static ref SCHEDULER: sync::Mutex<()> = sync::Mutex::new(());
	static ref DEPLOYED: sync::RwLock<Option<bool>> = sync::RwLock::new(None);
	static ref CONTEXT: sync::RwLock<Option<channel::Context>> = sync::RwLock::new(None);
	static ref ENV: sync::RwLock<Option<(Vec<ffi::OsString>, Vec<(ffi::OsString, ffi::OsString)>)>> =
		sync::RwLock::new(None);
	static ref RESOURCES: sync::RwLock<Option<Resources>> = sync::RwLock::new(None);
}
static mut HANDLE: Option<channel::Handle> = None;

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/// The sending half of a channel.
///
/// It has a synchronous blocking method [send()](Sender::send) and an asynchronous nonblocking method [selectable_send()](Sender::selectable_send).
pub struct Sender<T: serde::ser::Serialize>(Option<channel::Sender<T>>, Pid);
impl<T: serde::ser::Serialize> Sender<T> {
	/// Create a new `Sender<T>` with a remote [Pid]. This method returns instantly.
	pub fn new(remote: Pid) -> Sender<T> {
		if remote == pid() {
			panic!("Sender::<{}>::new() called with process's own pid. A process cannot create a channel to itself.", unsafe{intrinsics::type_name::<T>()});
		}
		let context = CONTEXT.read().unwrap();
		if let Some(sender) = channel::Sender::new(
			remote.addr(),
			context.as_ref().unwrap_or_else(|| {
				panic!("You must call init() immediately inside your application's main() function")
			}),
		) {
			Sender(Some(sender), remote)
		} else {
			panic!(
				"Sender::<{}>::new() called for pid {} when a Sender to this pid already exists",
				unsafe { intrinsics::type_name::<T>() },
				remote
			);
		}
	}

	/// Get the pid of the remote end of this Sender
	pub fn pid(&self) -> Pid {
		self.1
	}

	fn xxx_send<'a>(&'a self) -> Option<impl FnOnce(T) -> Result<(), ChannelError> + 'a>
	where
		T: 'static,
	{
		let context = CONTEXT.read().unwrap();
		self.0
			.as_ref()
			.unwrap()
			.xxx_send(context.as_ref().unwrap())
			.map(|x| {
				move |t: T| {
					let context = CONTEXT.read().unwrap();
					x(t, context.as_ref().unwrap())
				}
			})
	}

	/// Blocking send.
	pub fn send(&self, t: T) -> Result<(), ChannelError>
	where
		T: 'static,
	{
		self.0.as_ref().unwrap().send(t, &mut || {
			DerefMap(
				CONTEXT.read().unwrap(),
				|context| context.as_ref().unwrap(),
				marker::PhantomData,
			)
		})
	}

	/// [Selectable] send.
	///
	/// This needs to be passed to [select()](select) to be executed.
	pub fn selectable_send<'a, F: FnOnce() -> T + 'a, E: FnOnce(ChannelError) + 'a>(
		&'a self, send: F, err: E,
	) -> impl channel::Selectable + 'a
	where
		T: 'static,
	{
		self.0.as_ref().unwrap().selectable_send(send, err)
	}
}
#[doc(hidden)] // noise
impl<T: serde::ser::Serialize> ops::Drop for Sender<T> {
	fn drop(&mut self) {
		let context = CONTEXT.read().unwrap();
		self.0.take().unwrap().drop(context.as_ref().unwrap())
	}
}
impl<'a> io::Write for &'a Sender<u8> {
	#[inline(always)]
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		if buf.len() == 0 {
			return Ok(0);
		}
		self.send(buf[0]).map_err(|e| match e {
			ChannelError::Exited => io::ErrorKind::UnexpectedEof,
			ChannelError::Error => io::ErrorKind::ConnectionReset,
		})?;
		if buf.len() == 1 {
			return Ok(1);
		}
		for i in 1..buf.len() {
			if let Some(send) = self.xxx_send() {
				if let Ok(()) = send(buf[i]) {
				} else {
					return Ok(i);
				}
			} else {
				return Ok(i);
			}
		}
		Ok(buf.len())
	}

	#[inline(always)]
	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}
impl io::Write for Sender<u8> {
	#[inline(always)]
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		(&*self).write(buf)
	}

	#[inline(always)]
	fn flush(&mut self) -> io::Result<()> {
		(&*self).flush()
	}
}
impl<T: serde::ser::Serialize> fmt::Debug for Sender<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		self.0.fmt(f)
	}
}
impl<T: 'static + serde::ser::Serialize> futures::sink::Sink for Sender<T> {
	type SinkError = ChannelError;
	type SinkItem = T;

	fn poll_ready(
		&mut self, cx: &mut futures::task::Context,
	) -> Result<futures::Async<()>, Self::SinkError> {
		let context = CONTEXT.read().unwrap();
		self.0
			.as_mut()
			.unwrap()
			.futures_poll_ready(cx, context.as_ref().unwrap())
	}

	fn start_send(&mut self, item: Self::SinkItem) -> Result<(), Self::SinkError> {
		let context = CONTEXT.read().unwrap();
		self.0
			.as_mut()
			.unwrap()
			.futures_start_send(item, context.as_ref().unwrap())
	}

	fn poll_flush(
		&mut self, _cx: &mut futures::task::Context,
	) -> Result<futures::Async<()>, Self::SinkError> {
		Ok(futures::Async::Ready(()))
	}

	fn poll_close(
		&mut self, _cx: &mut futures::task::Context,
	) -> Result<futures::Async<()>, Self::SinkError> {
		Ok(futures::Async::Ready(()))
	}
}

/// The receiving half of a channel.
///
/// It has a synchronous blocking method [recv()](Receiver::recv) and an asynchronous nonblocking method [selectable_recv()](Receiver::selectable_recv).
pub struct Receiver<T: serde::de::DeserializeOwned>(Option<channel::Receiver<T>>, Pid);
impl<T: serde::de::DeserializeOwned> Receiver<T> {
	/// Create a new `Receiver<T>` with a remote [Pid]. This method returns instantly.
	pub fn new(remote: Pid) -> Receiver<T> {
		if remote == pid() {
			panic!("Receiver::<{}>::new() called with process's own pid. A process cannot create a channel to itself.", unsafe{intrinsics::type_name::<T>()});
		}
		let context = CONTEXT.read().unwrap();
		if let Some(receiver) = channel::Receiver::new(
			remote.addr(),
			context.as_ref().unwrap_or_else(|| {
				panic!("You must call init() immediately inside your application's main() function")
			}),
		) {
			Receiver(Some(receiver), remote)
		} else {
			panic!(
				"Sender::<{}>::new() called for pid {} when a Sender to this pid already exists",
				unsafe { intrinsics::type_name::<T>() },
				remote
			);
		}
	}

	/// Get the pid of the remote end of this Receiver
	pub fn pid(&self) -> Pid {
		self.1
	}

	fn xxx_recv<'a>(&'a self) -> Option<impl FnOnce() -> Result<T, ChannelError> + 'a>
	where
		T: 'static,
	{
		let context = CONTEXT.read().unwrap();
		self.0
			.as_ref()
			.unwrap()
			.xxx_recv(context.as_ref().unwrap())
			.map(|x| {
				move || {
					let context = CONTEXT.read().unwrap();
					x(context.as_ref().unwrap())
				}
			})
	}

	/// Blocking receive.
	pub fn recv(&self) -> Result<T, ChannelError>
	where
		T: 'static,
	{
		self.0.as_ref().unwrap().recv(&mut || {
			DerefMap(
				CONTEXT.read().unwrap(),
				|context| context.as_ref().unwrap(),
				marker::PhantomData,
			)
		})
	}

	/// [Selectable] receive.
	///
	/// This needs to be passed to [select()](select) to be executed.
	pub fn selectable_recv<'a, F: FnOnce(T) + 'a, E: FnOnce(ChannelError) + 'a>(
		&'a self, recv: F, err: E,
	) -> impl channel::Selectable + 'a
	where
		T: 'static,
	{
		self.0.as_ref().unwrap().selectable_recv(recv, err)
	}
}
#[doc(hidden)] // noise
impl<T: serde::de::DeserializeOwned> ops::Drop for Receiver<T> {
	fn drop(&mut self) {
		let context = CONTEXT.read().unwrap();
		self.0.take().unwrap().drop(context.as_ref().unwrap())
	}
}
impl<'a> io::Read for &'a Receiver<u8> {
	#[inline(always)]
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		if buf.len() == 0 {
			return Ok(0);
		}
		buf[0] = self.recv().map_err(|e| match e {
			ChannelError::Exited => io::ErrorKind::UnexpectedEof,
			ChannelError::Error => io::ErrorKind::ConnectionReset,
		})?;
		if buf.len() == 1 {
			return Ok(1);
		}
		for i in 1..buf.len() {
			if let Some(recv) = self.xxx_recv() {
				if let Ok(t) = recv() {
					buf[i] = t;
				} else {
					return Ok(i);
				}
			} else {
				return Ok(i);
			}
		}
		Ok(buf.len())
	}

	// #[inline(always)]
	// fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
	// }
	#[inline(always)]
	unsafe fn initializer(&self) -> io::Initializer {
		io::Initializer::nop()
	}
}
impl io::Read for Receiver<u8> {
	#[inline(always)]
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		(&*self).read(buf)
	}

	#[inline(always)]
	fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
		(&*self).read_exact(buf)
	}

	#[inline(always)]
	unsafe fn initializer(&self) -> io::Initializer {
		(&&*self).initializer()
	}
}
impl<T: serde::de::DeserializeOwned> fmt::Debug for Receiver<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		self.0.fmt(f)
	}
}
impl<T: 'static + serde::de::DeserializeOwned> futures::stream::Stream for Receiver<T> {
	type Error = ChannelError;
	type Item = T;

	fn poll_next(
		&mut self, cx: &mut futures::task::Context,
	) -> Result<futures::Async<Option<Self::Item>>, Self::Error> {
		let context = CONTEXT.read().unwrap();
		self.0
			.as_mut()
			.unwrap()
			.futures_poll_next(cx, context.as_ref().unwrap())
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/// `select()` lets you block on multiple blocking operations until progress can be made on at least one.
///
/// [Receiver::selectable_recv()](Receiver::selectable_recv) and [Sender::selectable_send()](Sender::selectable_send) let one create [Selectable] objects, any number of which can be passed to `select()`. `select()` then blocks until at least one is progressable, and then from any that are progressable picks one at random and executes it.
///
/// It returns an iterator of all the [Selectable] objects bar the one that has been executed.
///
/// It is inspired by the `select()` of go, which itself draws from David May's language [occam](https://en.wikipedia.org/wiki/Occam_(programming_language)) and Tony Hoare’s formalisation of [Communicating Sequential Processes](https://en.wikipedia.org/wiki/Communicating_sequential_processes).
pub fn select<'a>(
	select: Vec<Box<channel::Selectable + 'a>>,
) -> impl Iterator<Item = Box<channel::Selectable + 'a>> + 'a {
	channel::select(select, &mut || {
		DerefMap(
			CONTEXT.read().unwrap(),
			|context| context.as_ref().unwrap(),
			marker::PhantomData,
		)
	})
}
/// A thin wrapper around [select()](select) that loops until all [Selectable] objects have been executed.
pub fn run<'a>(mut select: Vec<Box<Selectable + 'a>>) {
	while select.len() != 0 {
		select = self::select(select).collect();
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/// Get the [Pid] of the current process
#[inline(always)]
pub fn pid() -> Pid {
	// TODO: panic!("You must call init() immediately inside your application's main() function")
	// TODO: cache
	let listener = unsafe { net::TcpListener::from_raw_fd(LISTENER_FD) };
	let local_addr = listener.local_addr().unwrap();
	listener.into_raw_fd();
	Pid::new(local_addr.ip(), local_addr.port())
}

/// Get the memory and CPU requirements configured at initialisation of the current process
pub fn resources() -> Resources {
	RESOURCES.read().unwrap().unwrap_or_else(|| {
		panic!("You must call init() immediately inside your application's main() function")
	})
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/// Spawn a new process.
///
/// `spawn()` takes 3 arguments:
///  * `start`: the function to be run in the new process
///  * `arg`: an argument that will be given to the `start` function
///  * `resources`: memory and CPU resource requirements of the new process
///
/// `spawn()` returns an Option<Pid>, which contains the [Pid] of the new process.
pub fn spawn<T: serde::ser::Serialize + serde::de::DeserializeOwned>(
	start: fn(parent: Pid, arg: T), arg: T, resources: Resources,
) -> Option<Pid> {
	let _scheduler = SCHEDULER.lock().unwrap();
	if !DEPLOYED.read().unwrap().unwrap_or_else(|| {
		panic!("You must call init() immediately inside your application's main() function")
	}) {
		// logln!("attempting spawn");
		let argv: Vec<ffi::CString> = ENV
			.read()
			.unwrap()
			.as_ref()
			.unwrap()
			.0
			.iter()
			.map(|x| ffi::CString::new(os::unix::ffi::OsStringExt::into_vec(x.clone())).unwrap())
			.collect(); // argv.split('\0').map(|x|ffi::CString::new(x).unwrap()).collect();
		let envp: Vec<(ffi::CString, ffi::CString)> = ENV
			.read()
			.unwrap()
			.as_ref()
			.unwrap()
			.1
			.iter()
			.map(|&(ref x, ref y)| {
				(
					ffi::CString::new(os::unix::ffi::OsStringExt::into_vec(x.clone())).unwrap(),
					ffi::CString::new(os::unix::ffi::OsStringExt::into_vec(y.clone())).unwrap(),
				)
			})
			.chain(iter::once((
				ffi::CString::new("DEPLOY_RESOURCES").unwrap(),
				ffi::CString::new(serde_json::to_string(&resources).unwrap()).unwrap(),
			)))
			.collect(); //envp.split('\0').map(|x|{let (a,b) = x.split_at(x.chars().position(|x|x=='=').unwrap_or_else(||panic!("invalid envp {:?}", x)));(ffi::CString::new(a).unwrap(),ffi::CString::new(&b[1..]).unwrap())}).collect();

		let our_pid = pid();

		let process_listener = nix::sys::socket::socket(
			nix::sys::socket::AddressFamily::Inet,
			nix::sys::socket::SockType::Stream,
			nix::sys::socket::SockFlag::SOCK_NONBLOCK,
			nix::sys::socket::SockProtocol::Tcp,
		).unwrap();
		nix::sys::socket::setsockopt(
			process_listener,
			nix::sys::socket::sockopt::ReuseAddr,
			&true,
		).unwrap();
		nix::sys::socket::bind(
			process_listener,
			&nix::sys::socket::SockAddr::Inet(nix::sys::socket::InetAddr::from_std(
				&net::SocketAddr::new("127.0.0.1".parse().unwrap(), 0),
			)),
		).unwrap();
		nix::sys::socket::setsockopt(
			process_listener,
			nix::sys::socket::sockopt::ReusePort,
			&true,
		).unwrap();
		let process_id = if let nix::sys::socket::SockAddr::Inet(inet) =
			nix::sys::socket::getsockname(process_listener).unwrap()
		{
			inet.to_std()
		} else {
			panic!()
		}.port();

		match nix::unistd::fork().expect("Fork failed") {
			nix::unistd::ForkResult::Child => {
				// let err = unsafe{nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL)}; assert_eq!(err, 0);
				unsafe {
					nix::sys::signal::sigaction(
						nix::sys::signal::SIGCHLD,
						&nix::sys::signal::SigAction::new(
							nix::sys::signal::SigHandler::SigDfl,
							nix::sys::signal::SaFlags::empty(),
							nix::sys::signal::SigSet::empty(),
						),
					).unwrap()
				};

				let valgrind_start_fd = if is_valgrind() {
					Some(valgrind_start_fd())
				} else {
					None
				};
				for fd in FdIter::new().filter(|&fd| {
					fd >= 3
						&& fd != process_listener
						&& (valgrind_start_fd.is_none() || fd < valgrind_start_fd.unwrap())
				}) {
					nix::unistd::close(fd).unwrap();
				}

				if process_listener != LISTENER_FD {
					let x = dup2(process_listener, LISTENER_FD).unwrap();
					assert_eq!(x, LISTENER_FD);
					nix::unistd::close(process_listener).unwrap();
				}

				let mut spawn_arg: Vec<u8> = Vec::new();
				let bridge_pid: Pid = BRIDGE.read().unwrap().unwrap();
				bincode::serialize_into(&mut spawn_arg, &bridge_pid).unwrap();
				bincode::serialize_into(&mut spawn_arg, &our_pid).unwrap();
				bincode::serialize_into(
					&mut spawn_arg,
					&((start_::<T> as fn(fn(Pid, T), Pid, Vec<u8>) as usize)
						.wrapping_sub(base as fn() -> ! as usize)),
				).unwrap(); // main as unsafe
				bincode::serialize_into(
					&mut spawn_arg,
					&((start as usize).wrapping_sub(base as fn() -> ! as usize)),
				).unwrap(); // main as unsafe
				bincode::serialize_into(&mut spawn_arg, &bincode::serialize(&arg).unwrap())
					.unwrap();
				let mut arg = unsafe {
					fs::File::from_raw_fd(
						memfd_create(&argv[0], nix::sys::memfd::MemFdCreateFlag::empty())
							.expect("Failed to memfd_create"),
					)
				};
				assert_eq!(arg.as_raw_fd(), ARG_FD);
				nix::unistd::ftruncate(arg.as_raw_fd(), spawn_arg.len() as i64).unwrap();
				arg.write_all(&spawn_arg).unwrap();
				let x =
					nix::unistd::lseek64(arg.as_raw_fd(), 0, nix::unistd::Whence::SeekSet).unwrap();
				assert_eq!(x, 0);

				let envp = envp
					.into_iter()
					.map(|(key, value)| {
						ffi::CString::new(format!(
							"{}={}",
							key.to_str().unwrap(),
							value.to_str().unwrap()
						)).unwrap()
					})
					.collect::<Vec<_>>();
				if !is_valgrind() {
					nix::unistd::execve(
						&ffi::CStr::from_bytes_with_nul(b"/proc/self/exe\0")
							.unwrap()
							.to_owned(),
						&argv,
						&envp,
					).expect("Failed to execve /proc/self/exe"); // or fexecve but on linux that uses proc also
				} else {
					let fd = nix::fcntl::open::<path::PathBuf>(
						&format!("/proc/self/fd/{}", valgrind_start_fd.unwrap()).into(),
						nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
						nix::sys::stat::Mode::empty(),
					).unwrap();
					let elf_desired_fd_ = valgrind_start_fd.unwrap() - 1;
					assert!(elf_desired_fd_ > fd);
					dup2(fd, elf_desired_fd_).unwrap();
					nix::unistd::close(fd).unwrap();
					nix::unistd::fexecve(elf_desired_fd_, &argv, &envp)
						.expect("Failed to execve /proc/self/fd/n");
				}
				unreachable!();
			}
			nix::unistd::ForkResult::Parent { child, .. } => child,
		};
		nix::unistd::close(process_listener).unwrap();
		let new_pid = Pid::new("127.0.0.1".parse().unwrap(), process_id);
		// BRIDGE.read().unwrap().as_ref().unwrap().0.send(ProcessOutputEvent::Spawn(new_pid)).unwrap();
		{
			let file = unsafe { fs::File::from_raw_fd(MONITOR_FD) };
			bincode::serialize_into(&mut &file, &ProcessOutputEvent::Spawn(new_pid)).unwrap();
			file.into_raw_fd();
		}
		return Some(new_pid);
	}
	let stream = unsafe { net::TcpStream::from_raw_fd(SCHEDULER_FD) };
	let (mut stream_read, mut stream_write) =
		(BufferedStream::new(&stream), BufferedStream::new(&stream));
	let mut stream_write_ = stream_write.write();
	let elf = if !is_valgrind() {
		fs::File::open("/proc/self/exe").unwrap()
	} else {
		unsafe {
			fs::File::from_raw_fd(
				nix::fcntl::open::<path::PathBuf>(
					&format!("/proc/self/fd/{}", valgrind_start_fd()).into(),
					nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
					nix::sys::stat::Mode::empty(),
				).unwrap(),
			)
		}
	};
	let len: u64 = elf.metadata().unwrap().len();
	bincode::serialize_into(&mut stream_write_, &resources).unwrap();
	bincode::serialize_into::<_, Vec<ffi::OsString>>(
		&mut stream_write_,
		&ENV.read().unwrap().as_ref().unwrap().0,
	).unwrap();
	bincode::serialize_into::<_, Vec<(ffi::OsString, ffi::OsString)>>(
		&mut stream_write_,
		&ENV.read().unwrap().as_ref().unwrap().1,
	).unwrap();
	bincode::serialize_into(&mut stream_write_, &len).unwrap();
	// copy(&mut &elf, &mut stream_write_, len as usize).unwrap();
	mem::drop(stream_write_);
	copy_sendfile(&**stream_write.get_ref(), &elf, len as usize).unwrap();
	let mut stream_write_ = stream_write.write();
	let mut arg_: Vec<u8> = Vec::new();
	let bridge_pid: Pid = BRIDGE.read().unwrap().unwrap();
	bincode::serialize_into(&mut arg_, &bridge_pid).unwrap();
	bincode::serialize_into(&mut arg_, &pid()).unwrap();
	bincode::serialize_into(
		&mut arg_,
		&((start_::<T> as fn(fn(Pid, T), Pid, Vec<u8>) as usize)
			.wrapping_sub(base as fn() -> ! as usize)),
	).unwrap(); // main as unsafe
	bincode::serialize_into(
		&mut arg_,
		&((start as usize).wrapping_sub(base as fn() -> ! as usize)),
	).unwrap(); // main as unsafe
			 // arg_.write_all(&arg).unwrap();
	let arg: Vec<u8> = bincode::serialize(&arg).unwrap();
	bincode::serialize_into(&mut arg_, &arg).unwrap();
	bincode::serialize_into(&mut stream_write_, &arg_).unwrap();
	mem::drop(stream_write_);
	let pid: Option<Pid> = bincode::deserialize_from(&mut stream_read)
		.map_err(map_bincode_err)
		.unwrap();
	mem::drop(stream_read);
	logln!("{} spawned? {}", self::pid(), pid.unwrap());
	if let Some(pid) = pid {
		let file = unsafe { fs::File::from_raw_fd(MONITOR_FD) };
		bincode::serialize_into(&mut &file, &ProcessOutputEvent::Spawn(pid)).unwrap();
		file.into_raw_fd();
	}
	stream.into_raw_fd();
	pid
}

fn start_<T: serde::de::DeserializeOwned>(start: fn(Pid, T), pid: Pid, arg: Vec<u8>) {
	start(pid, bincode::deserialize(&arg).unwrap())
}
#[inline(never)]
fn base() -> ! {
	unreachable!()
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

extern "C" fn fork_called() {
	// logln!("Fork called"); // TODO
}

extern "C" fn at_exit() {
	let handle = unsafe { HANDLE.take().unwrap() };
	mem::drop(handle);
	let mut context = CONTEXT.write().unwrap();
	context.take().unwrap();
	mem::forget(context);
}

#[doc(hidden)]
pub fn bridge_init() -> net::TcpListener {
	const BOUND_FD: os::unix::io::RawFd = 5; // from fabric
	if is_valgrind() {
		nix::unistd::close(valgrind_start_fd() - 1 - 12).unwrap();
	}
	// init();
	nix::sys::socket::listen(BOUND_FD, 100).unwrap();
	let listener = unsafe { net::TcpListener::from_raw_fd(BOUND_FD) };
	{
		let arg = unsafe { fs::File::from_raw_fd(ARG_FD) };
		let sched_arg: SchedulerArg = bincode::deserialize_from(&mut &arg).unwrap();
		drop(arg);
		let scheduler = net::TcpStream::connect(sched_arg.scheduler)
			.unwrap()
			.into_raw_fd();
		assert_eq!(scheduler, SCHEDULER_FD);

		let context = channel::Context::with_fd(LISTENER_FD, None);
		*CONTEXT.try_write().unwrap() = Some(context);
		let handle = channel::Context::tcp(&CONTEXT, |&_fd| None);
		*unsafe { &mut HANDLE } = Some(handle);
		let err = unsafe { nix::libc::atexit(at_exit) };
		assert_eq!(err, 0);
	}
	listener
}

/// Initialise the [deploy](self) runtime. This must be called immediately inside your application's `main()` function.
///
/// The `resources` argument describes memory and CPU requirements for the initial process.
pub fn init(mut resources: Resources) {
	if is_valgrind() {
		let _ = nix::unistd::close(valgrind_start_fd() - 1 - 12); // close non CLOEXEC'd fd of this binary
	}
	let envs_ = get_env::Env::new().collect::<Vec<_>>();
	let envs = Envs::from(&envs_);
	if envs
		.version
		.map(|x| x.expect("DEPLOY_VERSION must be 0 or 1"))
		.unwrap_or(false)
	{
		write!(io::stdout(), "deploy-lib {}", env!("CARGO_PKG_VERSION")).unwrap();
		process::exit(0);
	}
	if envs
		.recce
		.map(|x| x.expect("DEPLOY_RECCE must be 0 or 1"))
		.unwrap_or(false)
	{
		let file = unsafe { fs::File::from_raw_fd(3) };
		bincode::serialize_into(&file, &resources).unwrap();
		mem::drop(file);
		process::exit(0);
	}
	*ENV.write().unwrap() = Some((get_env::Arg::new().collect(), envs_));
	let format = envs
		.format
		.map(|x| x.expect("DEPLOY_FORMAT must be json or human"))
		.unwrap_or(Format::Human);
	let subprocess;
	let mut prog_vec;
	let bridge;
	{
		let deployed = envs.deploy == Some(Some(Deploy::Fabric));
		*DEPLOYED.write().unwrap() = Some(deployed);
		prog_vec = Vec::new();
		let mut scheduler = None;
		if !deployed {
			// logln!("native");
			if envs.resources.is_none() {
				// logln!("first");
				let our_process_listener = nix::sys::socket::socket(
					nix::sys::socket::AddressFamily::Inet,
					nix::sys::socket::SockType::Stream,
					nix::sys::socket::SockFlag::SOCK_NONBLOCK,
					nix::sys::socket::SockProtocol::Tcp,
				).unwrap();
				assert_eq!(our_process_listener, LISTENER_FD);
				nix::sys::socket::setsockopt(
					our_process_listener,
					nix::sys::socket::sockopt::ReuseAddr,
					&true,
				).unwrap();
				nix::sys::socket::bind(
					our_process_listener,
					&nix::sys::socket::SockAddr::Inet(nix::sys::socket::InetAddr::from_std(
						&net::SocketAddr::new("127.0.0.1".parse().unwrap(), 0),
					)),
				).unwrap();
				nix::sys::socket::setsockopt(
					our_process_listener,
					nix::sys::socket::sockopt::ReusePort,
					&true,
				).unwrap();
				let our_process_id = if let nix::sys::socket::SockAddr::Inet(inet) =
					nix::sys::socket::getsockname(our_process_listener).unwrap()
				{
					inet.to_std()
				} else {
					panic!()
				}.port();
				let bridge_process_listener = nix::sys::socket::socket(
					nix::sys::socket::AddressFamily::Inet,
					nix::sys::socket::SockType::Stream,
					nix::sys::socket::SockFlag::SOCK_NONBLOCK,
					nix::sys::socket::SockProtocol::Tcp,
				).unwrap();
				assert_eq!(bridge_process_listener, LISTENER_FD + 1);
				nix::sys::socket::setsockopt(
					bridge_process_listener,
					nix::sys::socket::sockopt::ReuseAddr,
					&true,
				).unwrap();
				nix::sys::socket::bind(
					bridge_process_listener,
					&nix::sys::socket::SockAddr::Inet(nix::sys::socket::InetAddr::from_std(
						&net::SocketAddr::new("127.0.0.1".parse().unwrap(), 0),
					)),
				).unwrap();
				nix::sys::socket::setsockopt(
					bridge_process_listener,
					nix::sys::socket::sockopt::ReusePort,
					&true,
				).unwrap();
				let bridge_process_id = if let nix::sys::socket::SockAddr::Inet(inet) =
					nix::sys::socket::getsockname(bridge_process_listener).unwrap()
				{
					inet.to_std()
				} else {
					panic!()
				}.port();
				let our_pid = Pid::new("127.0.0.1".parse().unwrap(), our_process_id);
				bridge = Pid::new("127.0.0.1".parse().unwrap(), bridge_process_id);
				if let nix::unistd::ForkResult::Parent { .. } = nix::unistd::fork().unwrap() {
					let err = unsafe { nix::libc::prctl(nix::libc::PR_SET_CHILD_SUBREAPER, 1) };
					assert_eq!(err, 0);
					// logln!("parent");

					nix::unistd::close(LISTENER_FD).unwrap();
					let err = dup2(LISTENER_FD + 1, LISTENER_FD).unwrap();
					assert_eq!(err, LISTENER_FD);

					let context = channel::Context::with_fd(LISTENER_FD, None);
					*CONTEXT.try_write().unwrap() = Some(context);

					let handle = channel::Context::tcp(&CONTEXT, move |&fd| {
						if let Ok(_remote) = nix::sys::socket::getpeername(fd).map(|remote| {
							if let nix::sys::socket::SockAddr::Inet(inet) = remote {
								inet.to_std()
							} else {
								panic!()
							}
						}) {
							// logln!("{}: {:?} != {:?}", pid(), remote, bridge.unwrap().addr());
							None
						} else {
							logln!("{}: getpeername failed", pid());
							None
						}
					});
					*unsafe { &mut HANDLE } = Some(handle);
					let err = unsafe { nix::libc::atexit(at_exit) };
					assert_eq!(err, 0);

					let x = thread_spawn(String::from("bridge-waitpid"), || {
						loop {
							match nix::sys::wait::waitpid(None, None) {
								Ok(nix::sys::wait::WaitStatus::Exited(_pid, code)) if code == 0 => {
									()
								} //assert_eq!(pid, child),
								// nix::sys::wait::WaitStatus::Signaled(pid, signal, _) if signal == nix::sys::signal::Signal::SIGKILL => assert_eq!(pid, child),
								Err(nix::Error::Sys(nix::errno::Errno::ECHILD)) => break,
								wait_status => {
									panic!("bad exit: {:?}", wait_status); /*loop {thread::sleep_ms(1000)}*/
								}
							}
						}
					});
					let mut exit_code = None;
					let mut formatter = if let Format::Human = format {
						Either::Left(Formatter::new(
							our_pid,
							if atty::is(atty::Stream::Stderr) {
								StyleSupport::EightBit
							} else {
								StyleSupport::None
							},
						))
					} else {
						Either::Right(io::stdout())
					};
					let mut processes = vec![(
						Sender::<ProcessInputEvent>::new(our_pid),
						Receiver::<ProcessOutputEvent>::new(our_pid),
					)];
					while processes.len() > 0 {
						// logln!("select");
						let mut event = None;
						let event_ = &::std::cell::RefCell::new(&mut event);

						select(
							processes
								.iter()
								.enumerate()
								.map(|(i, &(_, ref receiver))| {
									Box::new(receiver.selectable_recv(
										move |t: ProcessOutputEvent| {
											// logln!("ProcessOutputEvent {}: {:?}", i, t);
											**event_.borrow_mut() = Some((i, t));
										},
										|e| panic!("{:?}", e),
									)) as Box<Selectable>
								})
								.collect(),
						);
						// logln!("/select");
						mem::drop(event_);
						let (i, event): (usize, ProcessOutputEvent) = event.unwrap();
						let pid = processes[i].0.pid();
						let event = match event {
							ProcessOutputEvent::Spawn(new_pid) => {
								processes.push((
									Sender::<ProcessInputEvent>::new(new_pid),
									Receiver::<ProcessOutputEvent>::new(new_pid),
								));
								DeployOutputEvent::Spawn(pid, new_pid)
							}
							ProcessOutputEvent::Output(fd, output) => {
								// sender_.send(OutputEventInt::Output(pid, fd, output)).expect("send failed 1");
								// logln!("output: {:?} {:?}", fd, output);
								// print!("{}", output);
								DeployOutputEvent::Output(pid, fd, output)
							}
							ProcessOutputEvent::Exit(exit_code_) => {
								if exit_code_ != Either::Left(0) {
									if exit_code.is_none() {
										exit_code = Some(exit_code_); // TODO: nondeterministic
									}
								}
								processes.remove(i);
								DeployOutputEvent::Exit(pid, exit_code_)
							}
						};
						match &mut formatter {
							&mut Either::Left(ref mut formatter) => formatter.write(&event),
							&mut Either::Right(ref mut stdout) => {
								serde_json::to_writer(&mut *stdout, &event).unwrap();
								stdout.write_all(b"\n").unwrap()
							}
						}
					}
					x.join().unwrap();
					process::exit(exit_code.unwrap_or(Either::Left(0)).left().unwrap_or(1) as i32);
				}
				nix::unistd::close(LISTENER_FD + 1).unwrap();
				subprocess = false;
			// let err = unsafe{nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL)}; assert_eq!(err, 0);
			} else {
				// logln!("spawned");
				let arg = unsafe { fs::File::from_raw_fd(ARG_FD) };
				bridge = bincode::deserialize_from(&mut &arg)
					.map_err(map_bincode_err)
					.unwrap();
				(&arg).read_to_end(&mut prog_vec).unwrap();
				subprocess = true;
				resources = envs.resources.unwrap().unwrap();
			}
		} else {
			let arg = unsafe { fs::File::from_raw_fd(ARG_FD) };
			let sched_arg: SchedulerArg = bincode::deserialize_from(&mut &arg).unwrap();
			let bridge_arg: Pid = bincode::deserialize_from(&mut &arg).unwrap();
			(&arg).read_to_end(&mut prog_vec).unwrap();
			scheduler = Some(sched_arg.scheduler);
			subprocess = prog_vec.len() != 0;
			bridge = bridge_arg;
			if !subprocess {
				assert_eq!(resources, envs.resources.unwrap().unwrap());
			} else {
				resources = envs.resources.unwrap().unwrap();
			}
		}
		*RESOURCES.write().unwrap() = Some(resources);

		logln!(
			"PROCESS {}:{}: start setup; pid: {}",
			nix::unistd::getpid(),
			pid().addr().port(),
			pid()
		);
		*BRIDGE.write().unwrap() = Some(bridge);

		let fd = nix::fcntl::open(
			"/dev/null",
			nix::fcntl::OFlag::O_RDWR,
			nix::sys::stat::Mode::empty(),
		).unwrap(); // SCHEDULER
		assert_eq!(fd, 4);
		let err = dup2(fd, 5).unwrap(); // MONITOR
		assert_eq!(err, 5);
		let err = dup2(fd, 7).unwrap();
		assert_eq!(err, 7);
		let err = dup2(fd, 8).unwrap();
		assert_eq!(err, 8);
		let err = dup2(fd, 9).unwrap();
		assert_eq!(err, 9);

		let (bridge_sender, bridge_receiver) = mpsc::sync_channel::<ProcessOutputEvent>(0);
		let (z1_sender, z1_receiver) = mpsc::sync_channel::<Vec<u8>>(0);
		let y = forward_fd(nix::libc::STDOUT_FILENO, 8 - 1, bridge_sender.clone());
		const FORWARD_STDERR: bool = true;
		let z = if FORWARD_STDERR {
			let z = forward_fd(nix::libc::STDERR_FILENO, 9 - 1, bridge_sender.clone());
			Some(z)
		} else {
			None
		};
		let z1 = forward_input_fd(nix::libc::STDIN_FILENO, 10 - 1, z1_receiver);
		let (monitor_reader, monitor_writer) =
			nix::unistd::pipe2(nix::fcntl::OFlag::empty()).unwrap();
		assert_ne!(monitor_reader, MONITOR_FD - 1);
		let err = dup2(monitor_reader, MONITOR_FD - 1).unwrap();
		assert_eq!(err, MONITOR_FD - 1);
		nix::unistd::close(monitor_reader).unwrap();
		let monitor_reader = MONITOR_FD - 1;
		assert_ne!(monitor_writer, MONITOR_FD);
		let err = dup2(monitor_writer, MONITOR_FD).unwrap();
		assert_eq!(err, MONITOR_FD);
		nix::unistd::close(monitor_writer).unwrap();
		let monitor_writer = MONITOR_FD;

		let (socket_forwarder, socket_forwardee) = channel::socket_forwarder();
		let (reader, writer) = nix::unistd::pipe2(nix::fcntl::OFlag::empty()).unwrap();
		if let nix::unistd::ForkResult::Parent { child } = nix::unistd::fork().unwrap() {
			nix::unistd::close(monitor_writer).unwrap();
			assert_ne!(monitor_reader, MONITOR_FD);
			let err = dup2(monitor_reader, MONITOR_FD).unwrap();
			assert_eq!(err, MONITOR_FD);
			nix::unistd::close(monitor_reader).unwrap();
			nix::unistd::close(reader).unwrap();
			let fd = nix::fcntl::open(
				"/dev/null",
				nix::fcntl::OFlag::O_RDWR,
				nix::sys::stat::Mode::empty(),
			).unwrap();
			assert_ne!(fd, nix::libc::STDIN_FILENO);
			let err = dup2(fd, nix::libc::STDIN_FILENO).unwrap();
			assert_eq!(err, nix::libc::STDIN_FILENO);
			nix::unistd::close(fd).unwrap();
			let err = dup2(nix::libc::STDIN_FILENO, nix::libc::STDOUT_FILENO).unwrap();
			assert_eq!(err, nix::libc::STDOUT_FILENO);
			if FORWARD_STDERR {
				let err = dup2(nix::libc::STDIN_FILENO, nix::libc::STDERR_FILENO).unwrap();
				assert_eq!(err, nix::libc::STDERR_FILENO);
			}

			let context = channel::Context::with_socket_forwardee(socket_forwardee, pid().addr());
			*CONTEXT.try_write().unwrap() = Some(context);

			let handle = channel::Context::tcp(&CONTEXT, |&_fd| None);
			*unsafe { &mut HANDLE } = Some(handle);
			let err = unsafe { nix::libc::atexit(at_exit) };
			assert_eq!(err, 0);

			let sender = Sender::<ProcessOutputEvent>::new(bridge);
			let receiver = Receiver::<ProcessInputEvent>::new(bridge);

			let bridge_sender2 = bridge_sender.clone();
			let x3 = thread_spawn(String::from("monitor-monitorfd-to-channel"), move || {
				let file = unsafe { fs::File::from_raw_fd(MONITOR_FD) };
				loop {
					let event: Result<ProcessOutputEvent, _> =
						bincode::deserialize_from(&mut &file).map_err(map_bincode_err);
					if event.is_err() {
						break;
					}
					let event = event.unwrap();
					bridge_sender2.send(event).unwrap();
				}
				file.into_raw_fd();
			});

			let x = thread_spawn(String::from("monitor-channel-to-bridge"), move || {
				loop {
					let event = bridge_receiver.recv();
					// logln!("xxx OUTPUT {:?}", event);
					// if event.is_err() { // TODO: get rid of this, rely on ProcessOutputEvent::Exit coming thru
					// 	bincode::serialize_into(&mut sender, &ProcessOutputEvent::Exit(0)).unwrap(); // TODO: assert <= PIPE_BUF such that writing from parent process (after waitpid) is ok?
					// 	break;
					// }
					let event = event.unwrap();
					// bincode::serialize_into(&mut sender, &event).unwrap();
					sender.send(event.clone()).unwrap();
					if let ProcessOutputEvent::Exit(_) = event {
						// logln!("xxx exit");
						break;
					}
				}
			});
			let _x2 = thread_spawn(String::from("monitor-bridge-to-channel"), move || {
				loop {
					// let event: Result<ProcessInputEvent,_> = bincode::deserialize_from(&mut receiver).map_err(map_bincode_err);
					let event: Result<ProcessInputEvent, _> = receiver.recv();
					if event.is_err() {
						break;
					}
					let event = event.unwrap();
					match event {
						ProcessInputEvent::Input(fd, input) => {
							if fd == nix::libc::STDIN_FILENO {
								// logln!("xxx INPUT {:?} {}", input, input.len());
								z1_sender.send(input).unwrap();
							} else {
								unimplemented!()
							}
						}
						ProcessInputEvent::Kill => {
							// unsafe{intrinsics::abort()}
							nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL)
								.unwrap_or_else(|e| {
									assert_eq!(e, nix::Error::Sys(nix::errno::Errno::ESRCH))
								});
							break;
						}
					}
				}
			});
			nix::unistd::close(writer).unwrap();

			logln!(
				"PROCESS {}:{}: awaiting exit",
				nix::unistd::getpid(),
				pid().addr().port()
			);
			// logln!("awaiting exit");

			let exit = nix::sys::wait::waitpid(child, None).unwrap();
			logln!(
				"PROCESS {}:{}: exited {:?}",
				nix::unistd::getpid(),
				pid().addr().port(),
				exit
			);
			let _ = socket_forwarder.send(LISTENER_FD); // in case killed before forwarding listener
											   // logln!("EXITED!!!!! {:?}", exit);
			let code = match exit {
				nix::sys::wait::WaitStatus::Exited(pid, code) => {
					assert_eq!(pid, child);
					assert!(0 <= code);
					Either::Left(code as u8)
				}
				nix::sys::wait::WaitStatus::Signaled(pid, signal, _) => {
					assert_eq!(pid, child);
					Either::Right(unsafe { mem::transmute(signal) })
				}
				_ => panic!(),
			};
			// logln!("joining y");
			y.join().unwrap();
			// logln!("joining z");
			if FORWARD_STDERR {
				z.unwrap().join().unwrap();
			}
			// logln!("joining x3");
			x3.join().unwrap();
			bridge_sender.send(ProcessOutputEvent::Exit(code)).unwrap();
			mem::drop(bridge_sender);
			// logln!("joining x");
			x.join().unwrap();
			// nix::unistd::close(nix::libc::STDIN_FILENO).unwrap();
			// logln!("joining x2");
			// x2.join().unwrap();
			// logln!("joining z1");
			// z1.join().unwrap();
			// logln!("exiting");
			// unsafe{nix::libc::_exit(0)};
			process::exit(0);
		}
		// nix::unistd::close(monitor_reader).unwrap();
		assert_eq!(monitor_reader, SCHEDULER_FD);
		nix::unistd::close(writer).unwrap();
		nix::unistd::close(10 - 1).unwrap();
		if FORWARD_STDERR {
			nix::unistd::close(9 - 1).unwrap();
		}
		nix::unistd::close(8 - 1).unwrap();
		mem::forget(bridge_sender);
		mem::forget(bridge_receiver);
		mem::forget(z1_sender);
		// mem::forget(z1_receiver);
		mem::forget(y);
		if FORWARD_STDERR {
			mem::forget(z);
		}
		mem::forget(z1);
		let err = unsafe { nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL) };
		assert_eq!(err, 0);
		logln!("awaiting ready");
		let err = nix::unistd::read(reader, &mut [0]).unwrap();
		assert_eq!(err, 0);
		nix::unistd::close(reader).unwrap();
		logln!("ready");

		if deployed {
			let scheduler = net::TcpStream::connect(scheduler.unwrap())
				.unwrap()
				.into_raw_fd();
			assert_ne!(scheduler, SCHEDULER_FD);
			let err = dup2(scheduler, SCHEDULER_FD).unwrap();
			assert_eq!(err, SCHEDULER_FD);
			nix::unistd::close(scheduler).unwrap();
		}

		let err = unsafe {
			nix::libc::pthread_atfork(Some(fork_called), Some(fork_called), Some(fork_called))
		};
		assert_eq!(err, 0);
		// let heap_size = heap_size();
		// let err = unsafe{nix::libc::setrlimit64(nix::libc::RLIMIT_AS, &nix::libc::rlimit64{rlim_cur:heap_size as u64, rlim_max:heap_size as u64})}; assert_eq!(err, 0);
		// let err = unsafe{nix::libc::setrlimit64(nix::libc::RLIMIT_DATA, &nix::libc::rlimit64{rlim_cur:heap_size as u64, rlim_max:heap_size as u64})}; assert_eq!(err, 0);
		// let err = unsafe{nix::libc::setrlimit64(nix::libc::RLIMIT_MEMLOCK, &nix::libc::rlimit64{rlim_cur:heap_size as u64, rlim_max:heap_size as u64})}; assert_eq!(err, 0);
		// let err = unsafe{nix::libc::setrlimit64(nix::libc::RLIMIT_FSIZE, &nix::libc::rlimit64{rlim_cur:0, rlim_max:0})}; assert_eq!(err, 0);

		let context = channel::Context::with_fd(LISTENER_FD, Some(socket_forwarder.clone()));
		*CONTEXT.try_write().unwrap() = Some(context);

		let handle = channel::Context::tcp(&CONTEXT, move |&fd| {
			if let Ok(remote) = nix::sys::socket::getpeername(fd).map(|remote| {
				if let nix::sys::socket::SockAddr::Inet(inet) = remote {
					inet.to_std()
				} else {
					panic!()
				}
			}) {
				if remote != bridge.addr() {
					logln!("{}: {:?} != {:?}", pid(), remote, bridge.addr());
					None
				} else {
					logln!("{}: {:?} == {:?}", pid(), remote, bridge.addr());
					Some(socket_forwarder.clone())
				}
			} else {
				logln!("{}: getpeername failed", pid());
				None
			}
		});
		*unsafe { &mut HANDLE } = Some(handle);
		let err = unsafe { nix::libc::atexit(at_exit) };
		assert_eq!(err, 0);
	}

	unsafe {
		nix::sys::signal::sigaction(
			nix::sys::signal::SIGCHLD,
			&nix::sys::signal::SigAction::new(
				nix::sys::signal::SigHandler::SigIgn,
				nix::sys::signal::SaFlags::empty(),
				nix::sys::signal::SigSet::empty(),
			),
		).unwrap()
	};

	logln!(
		"PROCESS {}:{}: done setup; pid: {}; bridge: {:?}",
		nix::unistd::getppid(),
		pid().addr().port(),
		pid(),
		bridge
	);
	if !subprocess {
		return;
	} else {
		let (start_, start, parent, arg) = {
			let mut prog_arg = io::Cursor::new(&prog_vec);
			let parent: Pid = bincode::deserialize_from(&mut prog_arg)
				.map_err(map_bincode_err)
				.unwrap();
			let start_: usize = bincode::deserialize_from(&mut prog_arg)
				.map_err(map_bincode_err)
				.unwrap();
			let start_: fn(fn(Pid, ()), Pid, Vec<u8>) =
				unsafe { mem::transmute(start_.wrapping_add(base as fn() -> ! as usize)) };
			let start: usize = bincode::deserialize_from(&mut prog_arg)
				.map_err(map_bincode_err)
				.unwrap();
			let start: fn(Pid, ()) =
				unsafe { mem::transmute(start.wrapping_add(base as fn() -> ! as usize)) };
			let arg: Vec<u8> = bincode::deserialize_from(&mut prog_arg)
				.map_err(map_bincode_err)
				.unwrap();
			(start_, start, parent, arg)
		};
		start_(start, parent, arg);
		process::exit(0);
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

fn forward_fd(
	fd_: os::unix::io::RawFd, use_fd: os::unix::io::RawFd,
	bridge_sender: mpsc::SyncSender<ProcessOutputEvent>,
) -> thread::JoinHandle<()> {
	assert_ne!(fd_, use_fd);
	assert_ne!(
		nix::fcntl::fcntl(use_fd, nix::fcntl::FcntlArg::F_GETFD),
		Err(nix::Error::Sys(nix::errno::Errno::EBADF))
	);
	let (reader, writer) = nix::unistd::pipe2(nix::fcntl::OFlag::empty()).unwrap();
	assert_ne!(reader, writer);
	assert_ne!(writer, fd_);
	let fd = dup2(writer, fd_).unwrap();
	assert_eq!(fd, fd_);
	nix::unistd::close(writer).unwrap();
	if reader != use_fd {
		let fd = dup2(reader, use_fd).unwrap();
		assert_eq!(fd, use_fd);
		nix::unistd::close(reader).unwrap();
	}
	let reader = use_fd;
	nix::fcntl::fcntl(fd_, nix::fcntl::FcntlArg::F_GETFD).unwrap();
	nix::fcntl::fcntl(reader, nix::fcntl::FcntlArg::F_GETFD).unwrap();
	let reader = unsafe { fs::File::from_raw_fd(reader) };
	thread_spawn(String::from("monitor-forward_fd"), move || {
		let reader = reader;
		nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFD).unwrap();
		loop {
			let mut buf: [u8; 1024] = unsafe { mem::uninitialized() };
			let n = (&reader).read(&mut buf).unwrap();
			if n == 0 {
				mem::drop(reader);
				bridge_sender
					.send(ProcessOutputEvent::Output(fd_, Vec::new()))
					.unwrap();
				break;
			}
			bridge_sender
				.send(ProcessOutputEvent::Output(fd_, buf[..n].to_owned()))
				.unwrap();
		}
	})
}

fn forward_input_fd(
	fd_: os::unix::io::RawFd, use_fd: os::unix::io::RawFd, receiver: mpsc::Receiver<Vec<u8>>,
) -> thread::JoinHandle<()> {
	assert_ne!(fd_, use_fd);
	assert_ne!(
		nix::fcntl::fcntl(use_fd, nix::fcntl::FcntlArg::F_GETFD),
		Err(nix::Error::Sys(nix::errno::Errno::EBADF))
	);
	let (reader, writer) = nix::unistd::pipe2(nix::fcntl::OFlag::empty()).unwrap();
	assert_ne!(reader, fd_);
	let fd = dup2(reader, fd_).unwrap();
	assert_eq!(fd, fd_);
	nix::unistd::close(reader).unwrap();
	if writer != use_fd {
		let fd = dup2(writer, use_fd).unwrap();
		assert_eq!(fd, use_fd);
		nix::unistd::close(writer).unwrap();
	}
	let writer = use_fd;
	nix::fcntl::fcntl(fd_, nix::fcntl::FcntlArg::F_GETFD).unwrap();
	nix::fcntl::fcntl(writer, nix::fcntl::FcntlArg::F_GETFD).unwrap();
	let writer = unsafe { fs::File::from_raw_fd(writer) };
	thread_spawn(String::from("monitor-forward_input_fd"), move || {
		let writer = writer;
		nix::fcntl::fcntl(writer.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFD).unwrap();
		for input in receiver {
			if input.len() > 0 {
				if (&writer).write_all(&input).is_err() {
					mem::drop(writer);
					break;
				}
			} else {
				mem::drop(writer);
				break;
			}
		}
	})
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

mod get_env {
	use nix::libc;
	use std::{io::Read, *};

	extern "C" {
		#[linkage = "extern_weak"]
		static environ: *const *const *const libc::c_char;
		#[linkage = "extern_weak"]
		static __environ: *const *const *const libc::c_char;
	}
	// mac: https://github.com/rust-lang/rust/blob/c08480fce0f39f5c9c6db6dde0dccb375ca0ab14/src/libstd/sys/unix/os.rs#L396

	pub enum Env {
		Extern(*const *const libc::c_char),
		Iter(vec::IntoIter<(ffi::OsString, ffi::OsString)>),
	}
	impl Env {
		pub fn new() -> Env {
			let environ_ = unsafe {
				if !environ.is_null() {
					Some(*environ)
				} else if !__environ.is_null() {
					Some(*__environ)
				} else {
					None
				}
			};
			if let Some(environ_) = environ_ {
				Env::Extern(environ_)
			} else {
				let mut envp = Vec::new();
				fs::File::open("/proc/self/environ")
					.unwrap()
					.read_to_end(&mut envp)
					.unwrap(); // limited to 4096 bytes?
				if let Some(b'\0') = envp.last() {
					let null = envp.pop().unwrap();
					assert_eq!(null, b'\0');
				}
				Env::Iter(
					envp.split(|&x| x == b'\0')
						.flat_map(|x| {
							x.iter().skip(1).position(|&x| x == b'=').map(|position| {
								// https://github.com/rust-lang/rust/blob/c08480fce0f39f5c9c6db6dde0dccb375ca0ab14/src/libstd/sys/unix/os.rs#L434
								let (a, b) = x.split_at(position + 1);
								(
									os::unix::ffi::OsStringExt::from_vec(a.to_vec()),
									os::unix::ffi::OsStringExt::from_vec(b[1..].to_vec()),
								)
							})
						})
						.collect::<Vec<_>>()
						.into_iter(),
				)
			}
		}
	}
	impl Iterator for Env {
		type Item = (ffi::OsString, ffi::OsString);

		fn next(&mut self) -> Option<Self::Item> {
			unsafe {
				match self {
					&mut Env::Extern(ref mut environ_) => {
						loop {
							if !(**environ_).is_null() {
								let x = ffi::CStr::from_ptr(**environ_).to_bytes();
								*environ_ = environ_.offset(1);
								if let Some(x) = x.iter().skip(1).position(|&x| x == b'=').map(
									|position| {
										// https://github.com/rust-lang/rust/blob/c08480fce0f39f5c9c6db6dde0dccb375ca0ab14/src/libstd/sys/unix/os.rs#L434
										let (a, b) = x.split_at(position + 1);
										(
											os::unix::ffi::OsStringExt::from_vec(a.to_vec()),
											os::unix::ffi::OsStringExt::from_vec(b[1..].to_vec()),
										)
									},
								) {
									return Some(x);
								}
							} else {
								return None;
							}
						}
					}
					&mut Env::Iter(ref mut iter) => iter.next(),
				}
			}
		}
	}

	pub struct Arg(vec::IntoIter<ffi::OsString>);
	impl Arg {
		pub fn new() -> Arg {
			let mut argv = Vec::new();
			fs::File::open("/proc/self/cmdline")
				.unwrap()
				.read_to_end(&mut argv)
				.unwrap(); // limited to 4096 bytes?
			if let Some(b'\0') = argv.last() {
				let null = argv.pop().unwrap();
				assert_eq!(null, b'\0');
			}
			Arg(argv
				.split(|&x| x == b'\0')
				.map(|x| os::unix::ffi::OsStringExt::from_vec(x.to_vec()))
				.collect::<Vec<_>>()
				.into_iter())
		}
	}
	impl Iterator for Arg {
		type Item = ffi::OsString;

		fn next(&mut self) -> Option<Self::Item> {
			self.0.next()
		}
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

struct DerefMap<T, F: Fn(&T) -> &T1, T1>(T, F, ::std::marker::PhantomData<fn() -> T1>);
impl<T, F: Fn(&T) -> &T1, T1> ::std::ops::Deref for DerefMap<T, F, T1> {
	type Target = T1;

	fn deref(&self) -> &Self::Target {
		self.1(&self.0)
	}
}
