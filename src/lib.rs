//! Distributed programming primitives.
//!
//! This library provides a runtime to aide in the writing and debugging of distributed programs.
//!
//! The two key ideas are:
//!
//!  * **Spawning new processes:** The [`spawn()`](spawn) function can be used to spawn a new process running a particular closure.
//!  * **Channels:** [Sender]s and [Receiver]s can be used for synchronous or asynchronous inter-process communication.
//!
//! The only requirement to use is that [`init()`](init) must be called immediately inside your application's `main()` function.

#![doc(html_root_url = "https://docs.rs/constellation-rs/0.1.2")]
#![cfg_attr(feature = "nightly", feature(read_initializer))]
#![feature(async_await)]
#![warn(
	missing_copy_implementations,
	// missing_debug_implementations,
	missing_docs,
	trivial_casts,
	trivial_numeric_casts,
	unused_import_braces,
	unused_qualifications,
	unused_results,
	clippy::pedantic
)] // from https://github.com/rust-unofficial/patterns/blob/master/anti_patterns/deny-warnings.md
#![allow(
	clippy::inline_always,
	clippy::similar_names,
	clippy::if_not_else,
	clippy::module_name_repetitions,
	clippy::new_ret_no_self,
	clippy::type_complexity
)]

mod channel;

use either::Either;
use futures::{
	future::{FutureExt, TryFutureExt}, sink::{Sink, SinkExt}, stream::{Stream, StreamExt}
};
use log::trace;
use more_asserts::*;
use nix::{
	errno, fcntl, libc, sys::{
		signal, socket::{self, sockopt}, stat, wait
	}, unistd
};
use once_cell::sync::{Lazy, OnceCell};
use palaver::{
	env, file::{fd_path, fexecve}, socket::{socket as palaver_socket, SockFlag}, valgrind
};
use pin_utils::pin_mut;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
	borrow, convert::{Infallible, TryFrom, TryInto}, error::Error, ffi::{CString, OsString}, fmt, fs, future::Future, io::{self, Read, Write}, iter, marker, mem, net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream}, ops, os::unix::{
		ffi::OsStringExt, io::{AsRawFd, FromRawFd, IntoRawFd}
	}, path, pin::Pin, process, str, sync::{mpsc, Arc, Mutex, RwLock}, task::{Context, Poll}, thread::{self, Thread}
};

use constellation_internal::{
	file_from_reader, forbid_alloc, map_bincode_err, msg::{bincode_serialize_into, FabricRequest}, BufferedStream, Deploy, DeployOutputEvent, Envs, ExitStatus, Fd, Format, Formatter, PidInternal, ProcessInputEvent, ProcessOutputEvent, StyleSupport
};

pub use channel::ChannelError;
pub use constellation_internal::{Pid, Resources, RESOURCES_DEFAULT};

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

fn type_name<T: ?Sized>() -> &'static str {
	#[cfg(feature = "nightly")]
	{
		std::any::type_name::<T>()
	}
	#[cfg(not(feature = "nightly"))]
	{
		"<unknown>"
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/// Extension trait to provide convenient [`block()`](FutureExt1::block) method on futures.
///
/// Named `FutureExt1` to avoid clashing with [`futures::future::FutureExt`].
pub trait FutureExt1: Future {
	/// Convenience method over `futures::executor::block_on(future)`.
	fn block(self) -> Self::Output
	where
		Self: Sized,
	{
		// futures::executor::block_on(self) // Not reentrant for some reason
		struct ThreadNotify {
			thread: Thread,
		}
		impl futures::task::ArcWake for ThreadNotify {
			fn wake_by_ref(arc_self: &Arc<Self>) {
				arc_self.thread.unpark();
			}
		}
		let f = self;
		pin_mut!(f);
		let thread_notify = Arc::new(ThreadNotify {
			thread: thread::current(),
		});
		let waker = futures::task::waker_ref(&thread_notify);
		let mut cx = Context::from_waker(&waker);
		loop {
			if let Poll::Ready(t) = f.as_mut().poll(&mut cx) {
				return t;
			}
			thread::park();
		}
	}
}
impl<T: ?Sized> FutureExt1 for T where T: Future {}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

const LISTENER_FD: Fd = 3; // from fabric
const ARG_FD: Fd = 4; // from fabric
const SCHEDULER_FD: Fd = 4;
const MONITOR_FD: Fd = 5;

#[derive(Clone, Deserialize, Debug)]
struct SchedulerArg {
	ip: IpAddr,
	scheduler: Pid,
}

static PID: OnceCell<Pid> = OnceCell::new();
static BRIDGE: OnceCell<Pid> = OnceCell::new();
static DEPLOYED: OnceCell<bool> = OnceCell::new();
static RESOURCES: OnceCell<Resources> = OnceCell::new();
static SCHEDULER: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static REACTOR: Lazy<RwLock<Option<channel::Reactor>>> = Lazy::new(|| RwLock::new(None));
static HANDLE: Lazy<RwLock<Option<channel::Handle>>> = Lazy::new(|| RwLock::new(None));

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/// The sending half of a channel.
///
/// It has an async method [`send(value)`](Sender::send) and a nonblocking method [`try_send()`](Sender::try_send).
///
/// For blocking behaviour use [`.send(value).block()`](FutureExt1::block).
pub struct Sender<T: Serialize>(Option<channel::Sender<T>>, Pid);
impl<T: Serialize> Sender<T> {
	/// Create a new `Sender<T>` with a remote [Pid]. This method returns instantly.
	pub fn new(remote: Pid) -> Self {
		if remote == pid() {
			panic!("Sender::<{}>::new() called with process's own pid. A process cannot create a channel to itself.", type_name::<T>());
		}
		let context = REACTOR.read().unwrap();
		if let Some(sender) = channel::Sender::new(
			remote.addr(),
			context.as_ref().unwrap_or_else(|| {
				panic!("You must call init() immediately inside your application's main() function")
			}),
		) {
			Self(Some(sender), remote)
		} else {
			panic!(
				"Sender::<{}>::new() called for pid {} when a Sender to this pid already exists",
				type_name::<T>(),
				remote
			);
		}
	}

	/// Get the pid of the remote end of this Sender.
	pub fn remote_pid(&self) -> Pid {
		self.1
	}

	/// Nonblocking send.
	///
	/// If sending would not block, `Some` is returned with a FnOnce that accepts a `T` to send.
	/// If sending would block, `None` is returned.
	pub fn try_send<'a>(&'a self) -> Option<impl FnOnce(T) + 'a>
	where
		T: 'static,
	{
		let context = REACTOR.read().unwrap();
		self.0
			.as_ref()
			.unwrap()
			.try_send(BorrowMap::new(context, borrow_unwrap_option), None)
	}

	/// Send
	///
	/// This is an async fn.
	pub async fn send(&self, t: T)
	where
		T: 'static,
	{
		self.0.as_ref().unwrap().send(|| t).await;
	}

	/// Send
	///
	/// This is an async fn.
	pub async fn send_with(&self, f: impl FnOnce() -> T)
	where
		T: 'static,
	{
		self.0.as_ref().unwrap().send(f).await;
	}
}

#[doc(hidden)] // noise
impl<T: Serialize> Drop for Sender<T> {
	fn drop(&mut self) {
		let context = REACTOR.read().unwrap();
		self.0.take().unwrap().drop(context.as_ref().unwrap())
	}
}
impl<'a> Write for &'a Sender<u8> {
	#[inline(always)]
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		if buf.is_empty() {
			return Ok(0);
		}
		self.send(buf[0]).block();
		if buf.len() == 1 {
			return Ok(1);
		}
		for (i, buf) in (1..buf.len()).zip(buf[1..].iter().cloned()) {
			if let Some(send) = self.try_send() {
				send(buf);
			} else {
				return Ok(i);
			}
		}
		Ok(buf.len())
	}

	#[inline(always)]
	fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
		for &byte in buf {
			self.send(byte).block();
		}
		Ok(())
	}

	#[inline(always)]
	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}
impl Write for Sender<u8> {
	#[inline(always)]
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		(&*self).write(buf)
	}

	#[inline(always)]
	fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
		(&*self).write_all(buf)
	}

	#[inline(always)]
	fn flush(&mut self) -> io::Result<()> {
		(&*self).flush()
	}
}
impl<T: Serialize> fmt::Debug for Sender<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		self.0.fmt(f)
	}
}
impl<T: 'static + Serialize> Sink<T> for Sender<Option<T>> {
	type Error = Infallible;

	fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
		let context = REACTOR.read().unwrap();
		self.0
			.as_ref()
			.unwrap()
			.futures_poll_ready(cx, context.as_ref().unwrap())
	}

	fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
		let context = REACTOR.read().unwrap();
		self.0
			.as_ref()
			.unwrap()
			.futures_start_send(item, context.as_ref().unwrap())
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
		let context = REACTOR.read().unwrap();
		self.0
			.as_ref()
			.unwrap()
			.futures_poll_close(cx, context.as_ref().unwrap())
	}
}

impl<'a, T: Serialize + 'static, F: FnOnce() -> T> Future for channel::Send<'a, T, F> {
	type Output = ();

	fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
		let context = REACTOR.read().unwrap();
		self.futures_poll(cx, context.as_ref().unwrap())
	}
}

/// The receiving half of a channel.
///
/// It has an async method [`recv()`](Receiver::recv) and a nonblocking method [`try_recv()`](Receiver::try_recv).
///
/// For blocking behaviour use [`.recv().block()`](FutureExt1::block).
pub struct Receiver<T: DeserializeOwned>(Option<channel::Receiver<T>>, Pid);
impl<T: DeserializeOwned> Receiver<T> {
	/// Create a new `Receiver<T>` with a remote [Pid]. This method returns instantly.
	pub fn new(remote: Pid) -> Self {
		if remote == pid() {
			panic!("Receiver::<{}>::new() called with process's own pid. A process cannot create a channel to itself.", type_name::<T>());
		}
		let context = REACTOR.read().unwrap();
		if let Some(receiver) = channel::Receiver::new(
			remote.addr(),
			context.as_ref().unwrap_or_else(|| {
				panic!("You must call init() immediately inside your application's main() function")
			}),
		) {
			Self(Some(receiver), remote)
		} else {
			panic!(
				"Receiver::<{}>::new() called for pid {} when a Receiver to this pid already exists",
				type_name::<T>(),
				remote
			);
		}
	}

	/// Get the pid of the remote end of this Receiver.
	pub fn remote_pid(&self) -> Pid {
		self.1
	}

	/// Nonblocking recv.
	///
	/// If receiving would not block, `Some` is returned with a FnOnce that returns a `Result<T, ChannelError>`.
	/// If receiving would block, `None` is returned.
	pub fn try_recv<'a>(&'a self) -> Option<impl FnOnce() -> Result<T, ChannelError> + 'a>
	where
		T: 'static,
	{
		let context = REACTOR.read().unwrap();
		self.0
			.as_ref()
			.unwrap()
			.try_recv(BorrowMap::new(context, borrow_unwrap_option), None)
	}

	/// Receive.
	///
	/// This is an async fn.
	pub async fn recv(&self) -> Result<T, ChannelError>
	where
		T: 'static,
	{
		let mut x = None;
		self.0.as_ref().unwrap().recv(|y| x = Some(y)).await;
		x.unwrap()
	}
}
#[doc(hidden)] // noise
impl<T: DeserializeOwned> Drop for Receiver<T> {
	fn drop(&mut self) {
		let context = REACTOR.read().unwrap();
		self.0.take().unwrap().drop(context.as_ref().unwrap())
	}
}
impl<'a> Read for &'a Receiver<u8> {
	#[inline(always)]
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		if buf.is_empty() {
			return Ok(0);
		}
		buf[0] = self.recv().block().map_err(|e| match e {
			ChannelError::Exited => io::ErrorKind::UnexpectedEof,
			ChannelError::Unknown => io::ErrorKind::ConnectionReset,
			ChannelError::__Nonexhaustive => unreachable!(),
		})?;
		if buf.len() == 1 {
			return Ok(1);
		}
		for (i, buf) in (1..buf.len()).zip(buf[1..].iter_mut()) {
			if let Some(recv) = self.try_recv() {
				if let Ok(t) = recv() {
					*buf = t;
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
	fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
		for byte in buf {
			*byte = self.recv().block().map_err(|e| match e {
				ChannelError::Exited => io::ErrorKind::UnexpectedEof,
				ChannelError::Unknown => io::ErrorKind::ConnectionReset,
				ChannelError::__Nonexhaustive => unreachable!(),
			})?;
		}
		Ok(())
	}

	#[cfg(feature = "nightly")]
	#[inline(always)]
	unsafe fn initializer(&self) -> io::Initializer {
		io::Initializer::nop()
	}
}
impl Read for Receiver<u8> {
	#[inline(always)]
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		(&*self).read(buf)
	}

	#[inline(always)]
	fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
		(&*self).read_exact(buf)
	}

	#[cfg(feature = "nightly")]
	#[inline(always)]
	unsafe fn initializer(&self) -> io::Initializer {
		(&&*self).initializer()
	}
}
impl<T: DeserializeOwned> fmt::Debug for Receiver<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		self.0.fmt(f)
	}
}
impl<T: 'static + DeserializeOwned> Stream for Receiver<Option<T>> {
	type Item = Result<T, ChannelError>;

	fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
		let context = REACTOR.read().unwrap();
		self.0
			.as_ref()
			.unwrap()
			.futures_poll_next(cx, context.as_ref().unwrap())
	}
}

impl<'a, T: DeserializeOwned + 'static, F: FnOnce(Result<T, ChannelError>)> Future
	for channel::Recv<'a, T, F>
{
	type Output = ();

	fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
		let context = REACTOR.read().unwrap();
		self.futures_poll(cx, context.as_ref().unwrap())
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/// Get the [Pid] of the current process.
#[inline(always)]
pub fn pid() -> Pid {
	*PID.get().unwrap_or_else(|| {
		panic!("You must call init() immediately inside your application's main() function")
	})
}

/// Get the memory and CPU requirements configured at initialisation of the current process.
pub fn resources() -> Resources {
	*RESOURCES.get().unwrap_or_else(|| {
		panic!("You must call init() immediately inside your application's main() function")
	})
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

fn spawn_native(
	resources: Resources, f: serde_closure::FnOnce<(Vec<u8>,), fn((Vec<u8>,), (Pid,))>,
	_block: bool,
) -> Result<Pid, TrySpawnError> {
	trace!("spawn_native");
	let argv: Vec<CString> = env::args_os()
		.expect("Couldn't get argv")
		.iter()
		.map(|x| CString::new(OsStringExt::into_vec(x.clone())).unwrap())
		.collect(); // argv.split('\0').map(|x|CString::new(x).unwrap()).collect();
	let envp: Vec<(CString, CString)> = env::vars_os()
		.expect("Couldn't get envp")
		.iter()
		.map(|&(ref x, ref y)| {
			(
				CString::new(OsStringExt::into_vec(x.clone())).unwrap(),
				CString::new(OsStringExt::into_vec(y.clone())).unwrap(),
			)
		})
		.chain(iter::once((
			CString::new("CONSTELLATION_RESOURCES").unwrap(),
			CString::new(serde_json::to_string(&resources).unwrap()).unwrap(),
		)))
		.collect(); //envp.split('\0').map(|x|{let (a,b) = x.split_at(x.chars().position(|x|x=='=').unwrap_or_else(||panic!("invalid envp {:?}", x)));(CString::new(a).unwrap(),CString::new(&b[1..]).unwrap())}).collect();

	let our_pid = pid();

	let (process_listener, process_id) = native_process_listener();

	let mut spawn_arg: Vec<u8> = Vec::new();
	let bridge_pid: Pid = *BRIDGE.get().unwrap();
	bincode::serialize_into(&mut spawn_arg, &bridge_pid).unwrap();
	bincode::serialize_into(&mut spawn_arg, &our_pid).unwrap();
	bincode::serialize_into(&mut spawn_arg, &f).unwrap();

	let arg = file_from_reader(
		&mut &*spawn_arg,
		spawn_arg.len().try_into().unwrap(),
		&env::args_os().unwrap()[0],
		false,
	)
	.unwrap();

	let exe = CString::new(<OsString as OsStringExt>::into_vec(
		env::exe_path().unwrap().into(),
		// std::env::current_exe().unwrap().into(),
	))
	.unwrap();
	let envp = envp
		.into_iter()
		.map(|(key, value)| {
			CString::new(format!(
				"{}={}",
				key.to_str().unwrap(),
				value.to_str().unwrap()
			))
			.unwrap()
		})
		.collect::<Vec<_>>();
	let args_p = Vec::with_capacity(argv.len() + 1);
	let env_p = Vec::with_capacity(envp.len() + 1);

	let _child_pid = match unistd::fork().expect("Fork failed") {
		unistd::ForkResult::Child => {
			forbid_alloc(|| {
				// Memory can be in a weird state now. Imagine a thread has just taken out a lock,
				// but we've just forked. Lock still held. Avoid deadlock by doing nothing fancy here.
				// Including malloc.

				// let err = unsafe{libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL)}; assert_eq!(err, 0);
				unsafe {
					let _ = signal::sigaction(
						signal::SIGCHLD,
						&signal::SigAction::new(
							signal::SigHandler::SigDfl,
							signal::SaFlags::empty(),
							signal::SigSet::empty(),
						),
					)
					.unwrap();
				};

				let valgrind_start_fd = if valgrind::is().unwrap_or(false) {
					Some(valgrind::start_fd())
				} else {
					None
				};
				// FdIter uses libc::opendir which mallocs. Underlying syscall is getdents…
				for fd in (0..1024).filter(|&fd| {
					// FdIter::new().unwrap()
					fd >= 3
						&& fd != process_listener
						&& fd != arg.as_raw_fd() && (valgrind_start_fd.is_none()
						|| fd < valgrind_start_fd.unwrap())
				}) {
					let _ = unistd::close(fd); //.unwrap();
				}

				if process_listener != LISTENER_FD {
					palaver::file::move_fd(
						process_listener,
						LISTENER_FD,
						Some(fcntl::FdFlag::empty()),
						true,
					)
					.unwrap();
				}
				if arg.as_raw_fd() != ARG_FD {
					palaver::file::move_fd(
						arg.as_raw_fd(),
						ARG_FD,
						Some(fcntl::FdFlag::empty()),
						true,
					)
					.unwrap();
				}

				if !valgrind::is().unwrap_or(false) {
					#[inline]
					pub fn execve(
						path: &CString, args: &[CString], mut args_p: Vec<*const libc::c_char>,
						env: &[CString], mut env_p: Vec<*const libc::c_char>,
					) -> nix::Result<Infallible> {
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

						let _ =
							unsafe { libc::execve(path.as_ptr(), args_p.as_ptr(), env_p.as_ptr()) };

						Err(nix::Error::Sys(nix::errno::Errno::last()))
					}
					execve(&exe, &argv, args_p, &envp, env_p)
						.expect("Failed to execve /proc/self/exe"); // or fexecve but on linux that uses proc also
				} else {
					let fd = fcntl::open::<path::PathBuf>(
						&fd_path(valgrind_start_fd.unwrap()).unwrap(),
						fcntl::OFlag::O_RDONLY | fcntl::OFlag::O_CLOEXEC,
						stat::Mode::empty(),
					)
					.unwrap();
					let binary_desired_fd_ = valgrind_start_fd.unwrap() - 1;
					assert!(binary_desired_fd_ > fd);
					palaver::file::move_fd(
						fd,
						binary_desired_fd_,
						Some(fcntl::FdFlag::empty()),
						true,
					)
					.unwrap();
					fexecve(binary_desired_fd_, &argv, &envp)
						.expect("Failed to execve /proc/self/fd/n");
				}
				unreachable!();
			})
		}
		unistd::ForkResult::Parent { child, .. } => child,
	};
	unistd::close(process_listener).unwrap();
	drop(arg);
	let new_pid = Pid::new(IpAddr::V4(Ipv4Addr::LOCALHOST), process_id);
	// *BRIDGE.get().as_ref().unwrap().0.send(ProcessOutputEvent::Spawn(new_pid)).unwrap();
	{
		let file = unsafe { fs::File::from_raw_fd(MONITOR_FD) };
		bincode::serialize_into(&mut &file, &ProcessOutputEvent::Spawn(new_pid)).unwrap();
		let _ = file.into_raw_fd();
	}
	Ok(new_pid)
}

fn spawn_deployed(
	resources: Resources, f: serde_closure::FnOnce<(Vec<u8>,), fn((Vec<u8>,), (Pid,))>,
	_block: bool,
) -> Result<Pid, TrySpawnError> {
	trace!("spawn_deployed");
	let stream = unsafe { TcpStream::from_raw_fd(SCHEDULER_FD) };
	let (mut stream_read, mut stream_write) =
		(BufferedStream::new(&stream), BufferedStream::new(&stream));
	let mut arg: Vec<u8> = Vec::new();
	let bridge_pid: Pid = *BRIDGE.get().unwrap();
	bincode::serialize_into(&mut arg, &bridge_pid).unwrap();
	bincode::serialize_into(&mut arg, &pid()).unwrap();
	bincode::serialize_into(&mut arg, &f).unwrap();
	let binary = if !valgrind::is().unwrap_or(false) {
		env::exe().unwrap()
	} else {
		unsafe {
			fs::File::from_raw_fd(
				fcntl::open(
					&fd_path(valgrind::start_fd()).unwrap(),
					fcntl::OFlag::O_RDONLY | fcntl::OFlag::O_CLOEXEC,
					stat::Mode::empty(),
				)
				.unwrap(),
			)
		}
	};
	let request = FabricRequest {
		resources,
		bind: vec![],
		args: env::args_os().expect("Couldn't get argv"),
		vars: env::vars_os().expect("Couldn't get envp"),
		arg,
		binary,
	};
	bincode_serialize_into(&mut stream_write.write(), &request)
		.map_err(map_bincode_err)
		.unwrap();
	let pid: Option<Pid> = bincode::deserialize_from(&mut stream_read)
		.map_err(map_bincode_err)
		.unwrap();
	drop(stream_read);
	trace!("{} spawned? {}", self::pid(), pid.unwrap());
	if let Some(pid) = pid {
		let file = unsafe { fs::File::from_raw_fd(MONITOR_FD) };
		bincode::serialize_into(&mut &file, &ProcessOutputEvent::Spawn(pid)).unwrap();
		let _ = file.into_raw_fd();
	}
	let _ = stream.into_raw_fd();
	pid.ok_or(TrySpawnError::NoCapacity)
}

/// An error returned by the [`try_spawn()`](try_spawn) method detailing the reason if known.
#[allow(missing_copy_implementations)]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum TrySpawnError {
	/// [`try_spawn()`](try_spawn) failed because the new process couldn't be allocated.
	NoCapacity,
	/// [`try_spawn()`](try_spawn) failed for unknown reasons.
	Unknown,
	#[doc(hidden)]
	__Nonexhaustive, // https://github.com/rust-lang/rust/issues/44109
}

/// An error returned by the [`spawn()`](spawn) method detailing the reason if known.
#[allow(missing_copy_implementations)]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum SpawnError {
	/// [`spawn()`](spawn) failed for unknown reasons.
	Unknown,
	#[doc(hidden)]
	__Nonexhaustive,
}
impl From<SpawnError> for TrySpawnError {
	fn from(error: SpawnError) -> Self {
		match error {
			SpawnError::Unknown => Self::Unknown,
			SpawnError::__Nonexhaustive => unreachable!(),
		}
	}
}
impl TryFrom<TrySpawnError> for SpawnError {
	type Error = ();

	fn try_from(error: TrySpawnError) -> Result<Self, Self::Error> {
		match error {
			TrySpawnError::NoCapacity => Err(()),
			TrySpawnError::Unknown => Ok(Self::Unknown),
			TrySpawnError::__Nonexhaustive => unreachable!(),
		}
	}
}
impl fmt::Display for TrySpawnError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::NoCapacity => write!(
				f,
				"try_spawn() failed because the new process couldn't be allocated"
			),
			Self::Unknown => write!(f, "try_spawn() failed for unknown reasons"),
			Self::__Nonexhaustive => unreachable!(),
		}
	}
}
impl fmt::Display for SpawnError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Unknown => write!(f, "spawn() failed for unknown reasons"),
			Self::__Nonexhaustive => unreachable!(),
		}
	}
}
impl Error for TrySpawnError {}
impl Error for SpawnError {}

async fn spawn_inner<T: FnOnce(Pid) + Serialize + DeserializeOwned>(
	resources: Resources, start: T, block: bool,
) -> Result<Pid, TrySpawnError> {
	let _scheduler = SCHEDULER.lock().unwrap();
	let deployed = *DEPLOYED.get().unwrap_or_else(|| {
		panic!("You must call init() immediately inside your application's main() function")
	});
	let arg: Vec<u8> = bincode::serialize(&start).unwrap();

	let start: serde_closure::FnOnce<(Vec<u8>,), fn((Vec<u8>,), (Pid,))> = serde_closure::FnOnce!([arg]move|parent|{
		let arg: Vec<u8> = arg;
		let closure: T = bincode::deserialize(&arg).unwrap();
		closure(parent)
	});
	if !deployed {
		spawn_native(resources, start, block)
	} else {
		spawn_deployed(resources, start, block)
	}
}

/// Spawn a new process if it can be allocated immediately.
///
/// `try_spawn()` takes 2 arguments:
///  * `resources`: memory and CPU resource requirements of the new process
///  * `start`: the closure to be run in the new process
///
/// `try_spawn()` on success returns the [Pid] of the new process.
pub async fn try_spawn<T: FnOnce(Pid) + Serialize + DeserializeOwned>(
	resources: Resources, start: T,
) -> Result<Pid, TrySpawnError> {
	spawn_inner(resources, start, false).await
}

/// Spawn a new process.
///
/// `spawn()` takes 2 arguments:
///  * `resources`: memory and CPU resource requirements of the new process
///  * `start`: the closure to be run in the new process
///
/// `spawn()` on success returns the [Pid] of the new process.
pub async fn spawn<T: FnOnce(Pid) + Serialize + DeserializeOwned>(
	resources: Resources, start: T,
) -> Result<Pid, SpawnError> {
	spawn_inner(resources, start, true)
		.map_err(|err| err.try_into().unwrap())
		.await
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

extern "C" fn at_exit() {
	let handle = HANDLE.try_write().unwrap().take().unwrap();
	drop(handle);
	let mut context = REACTOR.write().unwrap();
	drop(context.take().unwrap());
}

#[doc(hidden)]
pub fn bridge_init() -> TcpListener {
	const BOUND_FD: Fd = 5; // from fabric
	if valgrind::is().unwrap_or(false) {
		unistd::close(valgrind::start_fd() - 1 - 12).unwrap();
	}
	// init();
	socket::listen(BOUND_FD, 100).unwrap();
	let listener = unsafe { TcpListener::from_raw_fd(BOUND_FD) };
	{
		let arg = unsafe { fs::File::from_raw_fd(ARG_FD) };
		let sched_arg: SchedulerArg = bincode::deserialize_from(&mut &arg).unwrap();
		drop(arg);
		let port = {
			let listener = unsafe { TcpListener::from_raw_fd(LISTENER_FD) };
			let local_addr = listener.local_addr().unwrap();
			let _ = listener.into_raw_fd();
			local_addr.port()
		};
		let our_pid = Pid::new(sched_arg.ip, port);
		PID.set(our_pid).unwrap();
		let scheduler = TcpStream::connect(sched_arg.scheduler.addr())
			.unwrap()
			.into_raw_fd();
		if scheduler != SCHEDULER_FD {
			palaver::file::move_fd(scheduler, SCHEDULER_FD, Some(fcntl::FdFlag::empty()), true)
				.unwrap();
		}

		let reactor = channel::Reactor::with_fd(LISTENER_FD);
		*REACTOR.try_write().unwrap() = Some(reactor);
		let handle = channel::Reactor::run(
			|| BorrowMap::new(REACTOR.read().unwrap(), borrow_unwrap_option),
			|&_fd| None,
		);
		*HANDLE.try_write().unwrap() = Some(handle);

		let err = unsafe { libc::atexit(at_exit) };
		assert_eq!(err, 0);
	}
	listener
}

fn native_bridge(format: Format, our_pid: Pid) -> Pid {
	let (bridge_process_listener, bridge_process_id) = native_process_listener();

	// No threads spawned between init and here so we're good
	assert_eq!(palaver::thread::count(), 1); // TODO: balks on 32 bit due to procinfo using usize that reflects target not host
	if let unistd::ForkResult::Parent { .. } = unistd::fork().unwrap() {
		#[cfg(any(target_os = "android", target_os = "linux"))]
		{
			let err = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) };
			assert_eq!(err, 0);
		}
		// trace!("parent");

		palaver::file::move_fd(
			bridge_process_listener,
			LISTENER_FD,
			Some(fcntl::FdFlag::empty()),
			false,
		)
		.unwrap();

		let bridge_pid = Pid::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bridge_process_id);
		PID.set(bridge_pid).unwrap();

		let reactor = channel::Reactor::with_fd(LISTENER_FD);
		*REACTOR.try_write().unwrap() = Some(reactor);
		let handle = channel::Reactor::run(
			|| BorrowMap::new(REACTOR.read().unwrap(), borrow_unwrap_option),
			move |&_fd| None,
		);
		*HANDLE.try_write().unwrap() = Some(handle);

		let err = unsafe { libc::atexit(at_exit) };
		assert_eq!(err, 0);

		let x = thread::Builder::new()
			.name(String::from("bridge-waitpid"))
			.spawn(|| {
				loop {
					match wait::waitpid(None, None) {
						Err(nix::Error::Sys(nix::errno::Errno::EINTR)) => (),
						Ok(wait::WaitStatus::Exited(_pid, code)) if code == 0 => (), //assert_eq!(pid, child),
						// wait::WaitStatus::Signaled(pid, signal, _) if signal == signal::Signal::SIGKILL => assert_eq!(pid, child),
						Err(nix::Error::Sys(errno::Errno::ECHILD)) => break,
						wait_status => {
							panic!("bad exit: {:?}", wait_status); /*loop {thread::sleep_ms(1000)}*/
						}
					}
				}
			})
			.unwrap();
		let mut exit_code = ExitStatus::Success;
		let (stdout, stderr) = (io::stdout(), io::stderr());
		let mut formatter = if let Format::Human = format {
			Either::Left(Formatter::new(
				our_pid,
				if atty::is(atty::Stream::Stderr) {
					StyleSupport::EightBit
				} else {
					StyleSupport::None
				},
				stdout.lock(),
				stderr.lock(),
			))
		} else {
			Either::Right(stdout.lock())
		};
		let mut processes = vec![(
			Sender::<ProcessInputEvent>::new(our_pid),
			Receiver::<ProcessOutputEvent>::new(our_pid),
		)];
		while !processes.is_empty() {
			let (event, i, _): (ProcessOutputEvent, usize, _) = futures::future::select_all(
				processes
					.iter()
					.map(|&(_, ref receiver)| receiver.recv().map(Result::unwrap).boxed_local()),
			)
			.block();
			let pid = processes[i].0.remote_pid();
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
					// trace!("output: {:?} {:?}", fd, output);
					// print!("{}", output);
					DeployOutputEvent::Output(pid, fd, output)
				}
				ProcessOutputEvent::Exit(exit_code_) => {
					exit_code += exit_code_;
					let _ = processes.remove(i);
					DeployOutputEvent::Exit(pid, exit_code_)
				}
			};
			match formatter {
				Either::Left(ref mut formatter) => formatter.write(&event),
				Either::Right(ref mut stdout) => {
					serde_json::to_writer(&mut *stdout, &event).unwrap();
					stdout.write_all(b"\n").unwrap()
				}
			}
		}
		x.join().unwrap();
		process::exit(exit_code.into());
	}
	unistd::close(bridge_process_listener).unwrap();
	Pid::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bridge_process_id)
}

fn native_process_listener() -> (Fd, u16) {
	let process_listener = palaver_socket(
		socket::AddressFamily::Inet,
		socket::SockType::Stream,
		SockFlag::SOCK_NONBLOCK,
		socket::SockProtocol::Tcp,
	)
	.unwrap();
	socket::setsockopt(process_listener, sockopt::ReuseAddr, &true).unwrap();
	socket::bind(
		process_listener,
		&socket::SockAddr::Inet(socket::InetAddr::from_std(&SocketAddr::new(
			IpAddr::V4(Ipv4Addr::LOCALHOST),
			0,
		))),
	)
	.unwrap();
	socket::setsockopt(process_listener, sockopt::ReusePort, &true).unwrap();
	let process_id =
		if let socket::SockAddr::Inet(inet) = socket::getsockname(process_listener).unwrap() {
			inet.to_std()
		} else {
			panic!()
		};
	assert_eq!(process_id.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));

	(process_listener, process_id.port())
}

fn monitor_process(
	bridge: Pid, deployed: bool,
) -> (channel::SocketForwardee, Fd, Fd, Option<Fd>, Fd) {
	const FORWARD_STDERR: bool = true;

	let (socket_forwarder, socket_forwardee) = channel::socket_forwarder();

	let (monitor_reader, monitor_writer) = unistd::pipe().unwrap(); // unistd::pipe2(fcntl::OFlag::empty())

	let (stdout_reader, stdout_writer) = unistd::pipe().unwrap();
	let (stderr_reader, stderr_writer) = if FORWARD_STDERR {
		let (stderr_reader, stderr_writer) = unistd::pipe().unwrap();
		(Some(stderr_reader), Some(stderr_writer))
	} else {
		(None, None)
	};
	let (stdin_reader, stdin_writer) = unistd::pipe().unwrap();

	let (reader, writer) = unistd::pipe().unwrap(); // unistd::pipe2(fcntl::OFlag::empty())

	// trace!("forking");
	// No threads spawned between init and here so we're good
	assert_eq!(palaver::thread::count(), 1); // TODO: balks on 32 bit due to procinfo using usize that reflects target not host
	if let unistd::ForkResult::Parent { child } = unistd::fork().unwrap() {
		unistd::close(reader).unwrap();
		unistd::close(monitor_writer).unwrap();
		unistd::close(stdout_writer).unwrap();
		if let Some(stderr_writer) = stderr_writer {
			unistd::close(stderr_writer).unwrap();
		}
		unistd::close(stdin_reader).unwrap();
		let (mut bridge_outbound_sender, mut bridge_outbound_receiver) =
			futures::channel::mpsc::channel::<ProcessOutputEvent>(0);
		let (bridge_inbound_sender, bridge_inbound_receiver) =
			mpsc::sync_channel::<ProcessInputEvent>(0);
		let stdout_thread = forward_fd(
			libc::STDOUT_FILENO,
			stdout_reader,
			bridge_outbound_sender.clone(),
		);
		let stderr_thread = stderr_reader.map(|stderr_reader| {
			forward_fd(
				libc::STDERR_FILENO,
				stderr_reader,
				bridge_outbound_sender.clone(),
			)
		});
		let stdin_thread =
			forward_input_fd(libc::STDIN_FILENO, stdin_writer, bridge_inbound_receiver);
		let fd = fcntl::open("/dev/null", fcntl::OFlag::O_RDWR, stat::Mode::empty()).unwrap();
		palaver::file::move_fd(fd, libc::STDIN_FILENO, Some(fcntl::FdFlag::empty()), false)
			.unwrap();
		palaver::file::copy_fd(
			libc::STDIN_FILENO,
			libc::STDOUT_FILENO,
			Some(fcntl::FdFlag::empty()),
			false,
		)
		.unwrap();
		if FORWARD_STDERR {
			palaver::file::copy_fd(
				libc::STDIN_FILENO,
				libc::STDERR_FILENO,
				Some(fcntl::FdFlag::empty()),
				false,
			)
			.unwrap();
		}

		let reactor = channel::Reactor::with_fd(LISTENER_FD);
		*REACTOR.try_write().unwrap() = Some(reactor);
		let handle = channel::Reactor::run(
			|| BorrowMap::new(REACTOR.read().unwrap(), borrow_unwrap_option),
			move |&fd| {
				if let Ok(remote) = socket::getpeername(fd).map(|remote| {
					if let socket::SockAddr::Inet(inet) = remote {
						inet.to_std()
					} else {
						panic!()
					}
				}) {
					if remote == bridge.addr() {
						trace!("{}: {:?} == {:?}", pid(), remote, bridge.addr());
						None
					} else {
						trace!("{}: {:?} != {:?}", pid(), remote, bridge.addr());
						Some(socket_forwarder.clone())
					}
				} else {
					trace!("{}: getpeername failed", pid());
					None
				}
			},
		);
		*HANDLE.try_write().unwrap() = Some(handle);

		let err = unsafe { libc::atexit(at_exit) };
		assert_eq!(err, 0);

		let sender = Sender::<ProcessOutputEvent>::new(bridge);
		let receiver = Receiver::<ProcessInputEvent>::new(bridge);

		let mut bridge_sender2 = bridge_outbound_sender.clone();
		let x3 = thread::Builder::new()
			.name(String::from("monitor-monitorfd-to-channel"))
			.spawn(move || {
				let file = unsafe { fs::File::from_raw_fd(monitor_reader) };
				loop {
					let event: Result<ProcessOutputEvent, _> =
						bincode::deserialize_from(&mut &file).map_err(map_bincode_err);
					if event.is_err() {
						break;
					}
					let event = event.unwrap();
					bridge_sender2.send(event).block().unwrap();
				}
				let _ = file.into_raw_fd();
			})
			.unwrap();

		let x = thread::Builder::new()
			.name(String::from("monitor-channel-to-bridge"))
			.spawn(move || {
				loop {
					let selected = match futures::future::select(
						bridge_outbound_receiver.next(),
						receiver.recv().boxed_local(),
					)
					.block()
					{
						futures::future::Either::Left((a, _)) => futures::future::Either::Left(a),
						futures::future::Either::Right((a, _)) => futures::future::Either::Right(a),
					};
					match selected {
						futures::future::Either::Left(event) => {
							let event = event.unwrap();
							sender.send(event.clone()).block();
							if let ProcessOutputEvent::Exit(_) = event {
								// trace!("xxx exit");
								break;
							}
						}
						futures::future::Either::Right(event) => {
							match event.unwrap() {
								ProcessInputEvent::Input(fd, input) => {
									// trace!("xxx INPUT {:?} {}", input, input.len());
									if fd == libc::STDIN_FILENO {
										bridge_inbound_sender
											.send(ProcessInputEvent::Input(fd, input))
											.unwrap();
									} else {
										unimplemented!()
									}
								}
								ProcessInputEvent::Kill => {
									// TODO: this is racey
									signal::kill(child, signal::Signal::SIGKILL).unwrap_or_else(
										|e| assert_eq!(e, nix::Error::Sys(errno::Errno::ESRCH)),
									);
								}
							}
						}
					}
				}
			})
			.unwrap();
		unistd::close(writer).unwrap();

		trace!(
			"PROCESS {}:{}: awaiting exit",
			unistd::getpid(),
			pid().addr().port()
		);
		// trace!("awaiting exit");

		let exit = loop {
			match wait::waitpid(child, None) {
				Err(nix::Error::Sys(nix::errno::Errno::EINTR)) => (),
				exit => break exit,
			}
		}
		.unwrap();
		// 		Ok(exit) => break exit,
		// if let Err(nix::Error::Sys(nix::errno::Errno::EINTR)) = exit {
		// 	loop {
		// 		thread::sleep(std::time::Duration::new(1,0));
		// 	}
		// }
		// if exit.is_err() {
		// 	loop {
		// 		thread::sleep(std::time::Duration::new(1,0));
		// 	}
		// }
		// let exit = exit.unwrap();
		trace!(
			"PROCESS {}:{}: exited {:?}",
			unistd::getpid(),
			pid().addr().port(),
			exit
		);
		#[cfg(not(any(
			target_os = "android",
			target_os = "freebsd",
			target_os = "linux",
			target_os = "netbsd",
			target_os = "openbsd"
		)))]
		{
			// use std::env;
			if deployed {
				// unistd::unlink(&env::current_exe().unwrap()).unwrap();
			}
		}
		let _ = deployed;

		let code = match exit {
			wait::WaitStatus::Exited(pid, code) => {
				assert_eq!(pid, child);
				ExitStatus::from_unix_status(code.try_into().unwrap())
			}
			wait::WaitStatus::Signaled(pid, signal, _) => {
				assert_eq!(pid, child);
				ExitStatus::from_unix_signal(signal)
			}
			_ => panic!(),
		};
		// trace!("joining stdout_thread");
		stdout_thread.join().unwrap();
		// trace!("joining stderr_thread");
		if FORWARD_STDERR {
			stderr_thread.unwrap().join().unwrap();
		}
		// trace!("joining x3");
		x3.join().unwrap();
		bridge_outbound_sender
			.send(ProcessOutputEvent::Exit(code))
			.block()
			.unwrap();
		drop(bridge_outbound_sender);
		// trace!("joining x");
		x.join().unwrap();
		// unistd::close(libc::STDIN_FILENO).unwrap();
		// trace!("joining x2");
		// trace!("joining stdin_thread");
		stdin_thread.join().unwrap();
		// trace!("exiting");
		// thread::sleep(std::time::Duration::from_millis(100));
		process::exit(0);
	}
	unistd::close(monitor_reader).unwrap();
	unistd::close(writer).unwrap();
	unistd::close(stdin_writer).unwrap();
	if FORWARD_STDERR {
		unistd::close(stderr_reader.unwrap()).unwrap();
	}
	unistd::close(stdout_reader).unwrap();
	#[cfg(any(target_os = "android", target_os = "linux"))]
	{
		let err = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
		assert_eq!(err, 0);
	}
	trace!("awaiting ready");
	let err = unistd::read(reader, &mut [0]).unwrap();
	assert_eq!(err, 0);
	unistd::close(reader).unwrap();
	trace!("ready");

	(
		socket_forwardee,
		monitor_writer,
		stdout_writer,
		stderr_writer,
		stdin_reader,
	)
}

/// Initialise the [constellation](self) runtime. This must be called immediately inside your application's `main()` function.
///
/// The `resources` argument describes memory and CPU requirements for the initial process.
pub fn init(resources: Resources) {
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
	assert_eq!(palaver::thread::count(), 1); // TODO: balks on 32 bit due to procinfo using usize that reflects target not host
	if valgrind::is().unwrap_or(false) {
		let _ = unistd::close(valgrind::start_fd() - 1 - 12); // close non CLOEXEC'd fd of this binary
	}
	let envs = Envs::from(&env::vars_os().expect("Couldn't get envp"));
	let version = envs
		.version
		.map_or(false, |x| x.expect("CONSTELLATION_VERSION must be 0 or 1"));
	let recce = envs
		.recce
		.map_or(false, |x| x.expect("CONSTELLATION_RECCE must be 0 or 1"));
	let format = envs.format.map_or(Format::Human, |x| {
		x.expect("CONSTELLATION_FORMAT must be json or human")
	});
	let deployed = envs.deploy == Some(Some(Deploy::Fabric));
	if version {
		assert!(!recce);
		println!("constellation-lib {}", env!("CARGO_PKG_VERSION"));
		process::exit(0);
	}
	if recce {
		let file = unsafe { fs::File::from_raw_fd(3) };
		bincode::serialize_into(&file, &resources).unwrap();
		drop(file);
		process::exit(0);
	}
	let (subprocess, resources, argument, bridge, scheduler, ip) = {
		if !deployed {
			if envs.resources.is_none() {
				(
					false,
					resources,
					vec![],
					None,
					None,
					IpAddr::V4(Ipv4Addr::LOCALHOST),
				)
			} else {
				let arg = unsafe { fs::File::from_raw_fd(ARG_FD) };
				let bridge = bincode::deserialize_from(&mut &arg)
					.map_err(map_bincode_err)
					.unwrap();
				let mut prog_arg = Vec::new();
				let _ = (&arg).read_to_end(&mut prog_arg).unwrap();
				(
					true,
					envs.resources.unwrap().unwrap(),
					prog_arg,
					Some(bridge),
					None,
					IpAddr::V4(Ipv4Addr::LOCALHOST),
				)
			}
		} else {
			let arg = unsafe { fs::File::from_raw_fd(ARG_FD) };
			let sched_arg: SchedulerArg = bincode::deserialize_from(&mut &arg).unwrap();
			let bridge: Pid = bincode::deserialize_from(&mut &arg).unwrap();
			let mut prog_arg = Vec::new();
			let _ = (&arg).read_to_end(&mut prog_arg).unwrap();
			let subprocess = !prog_arg.is_empty();
			if !subprocess {
				assert_eq!(resources, envs.resources.unwrap().unwrap());
			}
			(
				subprocess,
				envs.resources.unwrap().unwrap(),
				prog_arg,
				Some(bridge),
				Some(sched_arg.scheduler),
				sched_arg.ip,
			)
		}
	};

	// trace!(
	// 	"PROCESS {}:{}: start setup; pid: {}",
	// 	unistd::getpid(),
	// 	pid().addr().port(),
	// 	pid()
	// );

	let bridge = bridge.unwrap_or_else(|| {
		// We're in native topprocess
		let (our_process_listener, our_process_id) = native_process_listener();
		if our_process_listener != LISTENER_FD {
			palaver::file::move_fd(
				our_process_listener,
				LISTENER_FD,
				Some(fcntl::FdFlag::empty()),
				true,
			)
			.unwrap();
		}
		let our_pid = Pid::new(ip, our_process_id);
		native_bridge(format, our_pid)
		// let err = unsafe{libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL)}; assert_eq!(err, 0);
	});

	let port = {
		let listener = unsafe { TcpListener::from_raw_fd(LISTENER_FD) };
		let local_addr = listener.local_addr().unwrap();
		let _ = listener.into_raw_fd();
		local_addr.port()
	};
	let our_pid = Pid::new(ip, port);
	PID.set(our_pid).unwrap();

	trace!(
		"PROCESS {}:{}: start setup; pid: {}",
		unistd::getpid(),
		pid().addr().port(),
		pid()
	);

	DEPLOYED.set(deployed).unwrap();
	RESOURCES.set(resources).unwrap();
	BRIDGE.set(bridge).unwrap();

	let fd = fcntl::open("/dev/null", fcntl::OFlag::O_RDWR, stat::Mode::empty()).unwrap();
	if fd != SCHEDULER_FD {
		palaver::file::move_fd(fd, SCHEDULER_FD, Some(fcntl::FdFlag::empty()), true).unwrap();
	}
	palaver::file::copy_fd(SCHEDULER_FD, MONITOR_FD, Some(fcntl::FdFlag::empty()), true).unwrap();

	let (socket_forwardee, monitor_writer, stdout_writer, stderr_writer, stdin_reader) =
		monitor_process(bridge, deployed);
	assert_ne!(monitor_writer, MONITOR_FD);
	palaver::file::move_fd(
		monitor_writer,
		MONITOR_FD,
		Some(fcntl::FdFlag::empty()),
		false,
	)
	.unwrap();
	palaver::file::move_fd(
		stdout_writer,
		libc::STDOUT_FILENO,
		Some(fcntl::FdFlag::empty()),
		false,
	)
	.unwrap();
	if let Some(stderr_writer) = stderr_writer {
		palaver::file::move_fd(
			stderr_writer,
			libc::STDERR_FILENO,
			Some(fcntl::FdFlag::empty()),
			false,
		)
		.unwrap();
	}
	palaver::file::move_fd(
		stdin_reader,
		libc::STDIN_FILENO,
		Some(fcntl::FdFlag::empty()),
		false,
	)
	.unwrap();

	if deployed {
		let scheduler = TcpStream::connect(scheduler.unwrap().addr())
			.unwrap()
			.into_raw_fd();
		assert_ne!(scheduler, SCHEDULER_FD);
		palaver::file::move_fd(scheduler, SCHEDULER_FD, Some(fcntl::FdFlag::empty()), false)
			.unwrap();
	}

	let reactor = channel::Reactor::with_forwardee(socket_forwardee, pid().addr());
	*REACTOR.try_write().unwrap() = Some(reactor);
	let handle = channel::Reactor::run(
		|| BorrowMap::new(REACTOR.read().unwrap(), borrow_unwrap_option),
		|&_fd| None,
	);
	*HANDLE.try_write().unwrap() = Some(handle);

	let err = unsafe { libc::atexit(at_exit) };
	assert_eq!(err, 0);

	unsafe {
		let _ = signal::sigaction(
			signal::SIGCHLD,
			&signal::SigAction::new(
				signal::SigHandler::SigIgn,
				signal::SaFlags::empty(),
				signal::SigSet::empty(),
			),
		)
		.unwrap();
	};

	trace!(
		"PROCESS {}:{}: done setup; pid: {}; bridge: {:?}",
		unistd::getppid(),
		pid().addr().port(),
		pid(),
		bridge
	);

	if subprocess {
		let (start, parent) = {
			let mut argument = io::Cursor::new(&argument);
			let parent: Pid = bincode::deserialize_from(&mut argument)
				.map_err(map_bincode_err)
				.unwrap();
			let start: serde_closure::FnOnce<(Vec<u8>,), fn((Vec<u8>,), (Pid,))> =
				bincode::deserialize_from(&mut argument)
					.map_err(map_bincode_err)
					.unwrap();
			(start, parent)
		};
		start(parent);
		process::exit(0);
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

fn forward_fd(
	fd: Fd, reader: Fd, mut bridge_sender: futures::channel::mpsc::Sender<ProcessOutputEvent>,
) -> thread::JoinHandle<()> {
	thread::Builder::new()
		.name(String::from("monitor-forward_fd"))
		.spawn(move || {
			let reader = unsafe { fs::File::from_raw_fd(reader) };
			let _ = fcntl::fcntl(reader.as_raw_fd(), fcntl::FcntlArg::F_GETFD).unwrap();
			loop {
				let mut buf: [u8; 1024] = unsafe { mem::uninitialized() };
				let n = (&reader).read(&mut buf).unwrap();
				if n > 0 {
					bridge_sender
						.send(ProcessOutputEvent::Output(fd, buf[..n].to_owned()))
						.block()
						.unwrap();
				} else {
					drop(reader);
					bridge_sender
						.send(ProcessOutputEvent::Output(fd, Vec::new()))
						.block()
						.unwrap();
					break;
				}
			}
		})
		.unwrap()
}

fn forward_input_fd(
	fd: Fd, writer: Fd, receiver: mpsc::Receiver<ProcessInputEvent>,
) -> thread::JoinHandle<()> {
	thread::Builder::new()
		.name(String::from("monitor-forward_input_fd"))
		.spawn(move || {
			let mut writer = Some(unsafe { fs::File::from_raw_fd(writer) });
			let _ = fcntl::fcntl(
				writer.as_ref().unwrap().as_raw_fd(),
				fcntl::FcntlArg::F_GETFD,
			)
			.unwrap();
			for input in receiver {
				if writer.is_none() {
					continue;
				}
				match input {
					ProcessInputEvent::Input(fd_, ref input) if fd_ == fd => {
						if !input.is_empty() {
							if writer.as_ref().unwrap().write_all(input).is_err() {
								drop(writer.take().unwrap());
							}
						} else {
							drop(writer.take().unwrap());
							break;
						}
					}
					_ => unreachable!(),
				}
			}
		})
		.unwrap()
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

struct BorrowMap<T, F: Fn(&T) -> &T1, T1>(T, F, marker::PhantomData<fn() -> T1>);
impl<T, F: Fn(&T) -> &T1, T1> BorrowMap<T, F, T1> {
	fn new(t: T, f: F) -> Self {
		Self(t, f, marker::PhantomData)
	}
}
impl<T, F: Fn(&T) -> &T1, T1> borrow::Borrow<T1> for BorrowMap<T, F, T1> {
	fn borrow(&self) -> &T1 {
		self.1(&self.0)
	}
}
fn borrow_unwrap_option<T: ops::Deref<Target = Option<T1>>, T1>(x: &T) -> &T1 {
	x.as_ref().unwrap()
}
