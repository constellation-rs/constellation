mod circularbuffer;
mod heap;
mod serialize;
mod tcp_linux;

pub use self::tcp_linux::{socket_forwarder, SocketForwardee, SocketForwarder};
use self::tcp_linux::{EpollExecutor, EpollExecutorContext, LinuxTcp, LinuxTcpListener, Triggerer};
use either::Either;
use futures;
use nix;
use rand;
use serde;
use std::{
	cell,
	collections::HashMap,
	error, fmt, marker, mem, net, ops, os, ptr,
	sync::{self, Arc},
	thread,
};

fn getpid() -> nix::unistd::Pid {
	nix::unistd::getpid()
}
fn gettid() -> nix::unistd::Pid {
	nix::unistd::gettid()
}

pub struct Handle {
	trigger: Triggerer,
	tcp_thread: Option<thread::JoinHandle<()>>,
}
impl ops::Drop for Handle {
	fn drop(&mut self) {
		self.trigger.trigger();
		logln!(
			"{:?}: {:?}: self.tcp_thread.take().unwrap().join().unwrap()",
			nix::unistd::getpid(),
			::pid()
		);
		self.tcp_thread.take().unwrap().join().unwrap();
	}
}
pub struct Context {
	executor: EpollExecutor,
	listener: sync::RwLock<Option<LinuxTcpListener>>,
	pub sockets: sync::RwLock<HashMap<net::SocketAddr, Arc<sync::RwLock<Option<Channel>>>>>,
	local: net::SocketAddr,
	forward_listener: Option<SocketForwarder>,
}
impl Context {
	pub fn new(host: net::IpAddr) -> (Context, u16) {
		// logln!("printsockets: {:?}", (printsockets as extern "C" fn(&HashMap<net::SocketAddr,Arc<sync::RwLock<Option<Channel>>>>)) as *const ());
		let executor = EpollExecutor::new();
		let (listener, port) =
			LinuxTcpListener::new_ephemeral(&host, &executor.context(ptr::null()));
		let sockets = sync::RwLock::new(HashMap::new());
		let local = net::SocketAddr::new(host, port);
		(
			Context {
				executor,
				listener: sync::RwLock::new(Some(listener)),
				sockets,
				local,
				forward_listener: None,
			},
			port,
		)
	}

	pub fn with_fd(fd: os::unix::io::RawFd, forward_listener: Option<SocketForwarder>) -> Context {
		// logln!("printsockets: {:?}", (printsockets as extern "C" fn(&HashMap<net::SocketAddr,Arc<sync::RwLock<Option<Channel>>>>)) as *const ());
		let executor = EpollExecutor::new();
		let listener = LinuxTcpListener::with_fd(fd, &executor.context(ptr::null()));
		let sockets = sync::RwLock::new(HashMap::new());
		let local = if let nix::sys::socket::SockAddr::Inet(inet) =
			nix::sys::socket::getsockname(fd).unwrap()
		{
			inet.to_std()
		} else {
			panic!()
		};
		Context {
			executor,
			listener: sync::RwLock::new(Some(listener)),
			sockets,
			local,
			forward_listener,
		}
	}

	pub fn with_socket_forwardee(
		socket_forwardee: SocketForwardee, local: net::SocketAddr,
	) -> Context {
		// logln!("printsockets: {:?}", (printsockets as extern "C" fn(&HashMap<net::SocketAddr,Arc<sync::RwLock<Option<Channel>>>>)) as *const ());
		let executor = EpollExecutor::new();
		let listener = LinuxTcpListener::with_socket_forwardee(
			socket_forwardee,
			&executor.context(ptr::null()),
		);
		let sockets = sync::RwLock::new(HashMap::new());
		Context {
			executor,
			listener: sync::RwLock::new(Some(listener)),
			sockets,
			local,
			forward_listener: None,
		}
	}

	pub fn tcp<
		F: FnMut(&os::unix::io::RawFd) -> Option<SocketForwarder> + marker::Send + 'static,
	>(
		context: &'static sync::RwLock<Option<Context>>, mut accept_hook: F,
	) -> Handle {
		let (triggerer, triggeree) = {
			let context = context.try_read().unwrap();
			context
				.as_ref()
				.unwrap()
				.executor
				.context(1 as *const ())
				.add_trigger()
		};
		let tcp_thread = thread::Builder::new()
			.name("tcp-thread".to_string())
			.spawn(move || {
				let context = context.try_read().unwrap();
				let context = context.as_ref().unwrap();
				let mut xxx = context.listener.try_write().unwrap();
				let (executor, listener, sockets, local) = (
					&context.executor,
					xxx.as_mut().unwrap(),
					&context.sockets,
					&context.local,
				);
				let mut done: Option<
					sync::RwLockWriteGuard<
						HashMap<net::SocketAddr, Arc<sync::RwLock<Option<Channel>>>>,
					>,
				> = None;
				while done.is_none()
					|| done
						.as_ref()
						.unwrap()
						.iter()
						.filter(|&(_, ref inner)| {
							// TODO: maintain count
							match &inner.read().unwrap().as_ref().unwrap().inner {
					&Inner::Killed | &Inner::Closed/* | &Inner::Connecting(_)*/ => false,
					_a => {/*logln!("{:?}: laggard: {:?}", gettid(), a);*/ true},
				}
						})
						.count() > 0
				{
					if let &Some(ref socket) = &done {
						logln!(
							"{:?}: {:?}: socket: {:?}",
							nix::unistd::getpid(),
							::pid(),
							&**socket
						);
					}
					executor.wait(|executor, _events, data| {
						if data == ptr::null() {
							for (connectee, remote, executor_hack) in
								listener.poll(&executor.context(ptr::null()), &mut accept_hook)
							{
								assert!(!ord(&local, &remote));
								let is_done = done.is_some();
								let mut sockets_ = if done.is_none() {
									Some(sockets.write().unwrap())
								} else {
									None
								};
								let sockets = done
									.as_mut()
									.map(|x| &mut **x)
									.unwrap_or_else(|| &mut *sockets_.as_mut().unwrap());
								if sockets.contains_key(&remote) {
									let channel_ = &**sockets.get(&remote).unwrap();
									if let &Inner::Connected(ref e) =
										&channel_.read().unwrap().as_ref().unwrap().inner
									{
										logln!("!!!***!!!*** {:?} {:?} {:?}", e, local, remote);
										continue;
									}
									let executor_key: *const sync::RwLock<Option<Channel>> = channel_;
									let executor = &executor.context(executor_key as *const ());
									executor_hack(executor, &connectee);
									let mut channel = channel_.write().unwrap();
									let channel = channel.as_mut().unwrap();
									match &mut channel.inner {
										&mut Inner::Connecting(ref mut connecting) => {
											// assert!(connecting.incoming.is_none(), "{:?}", connecting); // been hit
											connecting.incoming = Some(connectee);
											assert!(connecting.outgoing.is_none());
											// connecting.outgoing = LinuxTcpConnecter::new(local, remote);
										}
										&mut Inner::ConnectingLocalClosed(ref mut connecting) => {
											// assert!(connecting.incoming.is_none(), "{:?}", connecting); // been hit
											connecting.incoming = Some(connectee);
											assert!(connecting.outgoing.is_none());
											// connecting.outgoing = LinuxTcpConnecter::new(local, remote);
										}
										&mut Inner::Closed /*| &mut Inner::Killed*/ => {
											// *inner = Inner::Connecting(InnerConnecting::new(local.clone(), remote, Some(connectee), false));
											let mut inner = Inner::connect(
												local.clone(),
												remote,
												Some(connectee),
												executor,
											);
											if is_done {
												if inner.closable() {
													inner.close(executor);
												}
											}
											if !inner.closed() {
												channel.inner = inner;
											}
										}
										e => panic!("{:?} {:?} {:?}", e, local, remote),
									}
									channel.inner.poll(executor);
									if !is_done {
										for sender in channel.senders.values() {
											sender.unpark(); // TODO: don't do unless actual progress
										}
										for sender_future in channel.senders_futures.drain(..) {
											sender_future.wake();
										}
										for receiver in channel.receivers.values() {
											receiver.unpark(); // TODO: don't do unless actual progress
										}
										for receiver_future in channel.receivers_futures.drain(..) {
											receiver_future.wake();
										}
									} else {
										if channel.inner.closable() {
											channel.inner.close(executor);
										}
									}
								} else {
									let channel = Arc::new(sync::RwLock::new(None));
									let executor_key: *const sync::RwLock<Option<Channel>> = &*channel;
									let executor = &executor.context(executor_key as *const ());
									executor_hack(executor, &connectee);
									let mut inner = Inner::connect(
										local.clone(),
										remote,
										Some(connectee),
										executor,
									);
									if is_done {
										if inner.closable() {
											inner.close(executor);
										}
									}
									if !inner.closed() {
										*channel.try_write().unwrap() = Some(Channel::new(inner));
										let x = sockets.insert(remote, channel);
										assert!(x.is_none());
									}
								}
							}
						} else if data != 1 as *const () {
							if done.is_none() {
								let mut sockets = sockets.write().unwrap();
								let executor_key: *const sync::RwLock<Option<Channel>> = data as *const _;
								let executor = &executor.context(executor_key as *const ());
								// assert!(sockets.values().any(|channel|{
								// 	let executor_key2: *const sync::RwLock<Option<Channel>> = &**channel;
								// 	executor_key2 == executor_key
								// }), "{:?}:{:?} {:?}", getpid(), gettid(), executor_key);
								// let mut channel = unsafe{&*executor_key}.write().unwrap();
								let channel_arc = sockets.values().find(|&channel| {
									let executor_key2: *const sync::RwLock<Option<Channel>> = &**channel;
									executor_key2 == executor_key
								});
								if let Some(channel_arc) = channel_arc {
									// TODO
									let mut channel = channel_arc.write().unwrap();
									assert_eq!(
										sync::Arc::strong_count(&channel_arc),
										1
											+ channel.as_ref().unwrap().senders_count
											+ channel.as_ref().unwrap().receivers_count,
										"{:?}:{:?} {:?}",
										getpid(),
										gettid(),
										executor_key
									);
									if {
										let channel: &mut Channel = channel.as_mut().unwrap();
										let inner: &mut Inner = &mut channel.inner;
										inner.poll(executor);
										for sender in channel.senders.values() {
											sender.unpark(); // TODO: don't do unless actual progress
										}
										for sender_future in channel.senders_futures.drain(..) {
											sender_future.wake();
										}
										for receiver in channel.receivers.values() {
											receiver.unpark(); // TODO: don't do unless actual progress
										}
										for receiver_future in channel.receivers_futures.drain(..) {
											receiver_future.wake();
										}
										// logln!("unk: {:?}, {:?}", data, events);
										channel.senders_count == 0
											&& channel.receivers_count == 0
											&& inner.closed()
									} {
										let x = channel.take().unwrap();
										assert!(
											x.senders_count == 0
												&& x.receivers_count == 0
												&& x.inner.closed()
										);
										let key = sockets
											.iter()
											.find(|&(_key, channel)| {
												let executor_key2: *const sync::RwLock<Option<Channel>> = &**channel;
												executor_key2 == executor_key
											})
											.unwrap()
											.0
											.clone();
										mem::drop(channel);
										logln!(
											"{:?}:{:?} drop epoll1 {:?}",
											getpid(),
											gettid(),
											executor_key
										);
										let mut x = Arc::try_unwrap(sockets.remove(&key).unwrap())
											.expect("channel.try_unwrap epoll 1");
										assert!(x.get_mut().unwrap().is_none());
										logln!("channel.try_unwrap epoll 1 success");
									}
								}
							} else {
								let sockets = &mut **done.as_mut().unwrap();
								let executor_key: *const sync::RwLock<Option<Channel>> = data as *const _;
								let executor = &executor.context(executor_key as *const ());
								// assert!(sockets.values().any(|channel|{
								// 	let executor_key2: *const sync::RwLock<Option<Channel>> = &**channel;
								// 	executor_key2 == executor_key
								// }), "{:?}:{:?} {:?}", getpid(), gettid(), executor_key);
								// let mut channel = unsafe{&*executor_key}.write().unwrap();
								let channel_arc = sockets.values().find(|&channel| {
									let executor_key2: *const sync::RwLock<Option<Channel>> = &**channel;
									executor_key2 == executor_key
								});
								if let Some(channel_arc) = channel_arc {
									// TODO
									let mut channel = channel_arc.write().unwrap();
									assert_eq!(
										sync::Arc::strong_count(&channel_arc),
										1
											+ channel.as_ref().unwrap().senders_count
											+ channel.as_ref().unwrap().receivers_count,
										"{:?}:{:?} {:?}",
										getpid(),
										gettid(),
										executor_key
									);
									if {
										let channel: &mut Channel = channel.as_mut().unwrap();
										let inner: &mut Inner = &mut channel.inner;
										inner.poll(executor);
										if inner.closable() {
											inner.close(executor);
										}
										channel.senders_count == 0
											&& channel.receivers_count == 0
											&& inner.closed()
									} {
										let x = channel.take().unwrap();
										assert!(
											x.senders_count == 0
												&& x.receivers_count == 0
												&& x.inner.closed()
										);
										let key = sockets
											.iter()
											.find(|&(_key, channel)| {
												let executor_key2: *const sync::RwLock<Option<Channel>> = &**channel;
												executor_key2 == executor_key
											})
											.unwrap()
											.0
											.clone();
										mem::drop(channel);
										logln!(
											"{:?}:{:?} drop epoll2 {:?}",
											getpid(),
											gettid(),
											executor_key
										);
										let mut x = Arc::try_unwrap(sockets.remove(&key).unwrap())
											.expect("channel.try_unwrap epoll 2");
										assert!(x.get_mut().unwrap().is_none());
										logln!("channel.try_unwrap epoll 2 success");
									}
								}
							}
						} else {
							assert!(done.is_none());
							logln!("{:?}: {}: \\close", nix::unistd::getpid(), ::pid());
							triggeree.triggered();
							done = Some(sockets.write().unwrap());
							let sockets = &mut **done.as_mut().unwrap();
							for inner in sockets.values_mut() {
								let executor_key: *const sync::RwLock<Option<Channel>> = &**inner;
								let executor = &executor.context(executor_key as *const ());
								let mut channel = inner.write().unwrap();
								let channel: &mut Channel = channel.as_mut().unwrap();
								let inner: &mut Inner = &mut channel.inner;
								if inner.closable() {
									inner.close(executor);
								}
							}
						}
					});
				}
				logln!("{:?}: {}: /close", nix::unistd::getpid(), ::pid());
			})
			.unwrap();
		Handle {
			trigger: triggerer,
			tcp_thread: Some(tcp_thread),
		}
	}
}
impl ops::Drop for Context {
	fn drop(&mut self) {
		logln!("{}:{}: drop context", getpid(), gettid());
		if let &Some(ref forward_listener) = &self.forward_listener {
			logln!("{}:{}: %%% sending listener", getpid(), gettid());
			forward_listener
				.send(self.listener.try_write().unwrap().take().unwrap().into_fd())
				.unwrap();
		}
	}
}

#[derive(Debug)]
pub struct Channel {
	inner: Inner,
	senders_count: usize,
	receivers_count: usize,
	senders: HashMap<thread::ThreadId, thread::Thread>, // TODO: linked list
	senders_futures: Vec<futures::task::Waker>,
	receivers: HashMap<thread::ThreadId, thread::Thread>,
	receivers_futures: Vec<futures::task::Waker>,
}
impl Channel {
	fn new(inner: Inner) -> Channel {
		Channel {
			inner,
			senders_count: 0,
			receivers_count: 0,
			senders: HashMap::new(),
			senders_futures: Vec::new(),
			receivers: HashMap::new(),
			receivers_futures: Vec::new(),
		}
	}
}

#[derive(Debug)]
enum Inner {
	Connecting(InnerConnecting),
	ConnectingLocalClosed(InnerConnectingLocalClosed),
	Connected(InnerConnected),
	RemoteClosed(InnerConnectedRemoteClosed),
	LocalClosed(InnerConnectedLocalClosed),
	Closing(InnerConnectedClosing),
	Closed,
	Killed,
	Xxx,
}
impl Inner {
	pub fn connect(
		local: net::SocketAddr, remote: net::SocketAddr, incoming: Option<LinuxTcp>,
		executor: &EpollExecutorContext,
	) -> Self {
		match InnerConnecting::new(local, remote, incoming, executor) {
			Either::Left(connecting) => Inner::Connecting(connecting),
			Either::Right(Some(Either::Left(connected))) => Inner::Connected(connected),
			Either::Right(Some(Either::Right(remote_closed))) => Inner::RemoteClosed(remote_closed),
			Either::Right(None) => Inner::Killed,
		}
	}

	pub fn poll(&mut self, executor: &EpollExecutorContext) {
		*self = match mem::replace(self, Inner::Xxx) {
			Inner::Connecting(connecting) => match connecting.poll(executor) {
				Either::Left(connecting) => Inner::Connecting(connecting),
				Either::Right(Some(Either::Left(connected))) => Inner::Connected(connected),
				Either::Right(Some(Either::Right(remote_closed))) => {
					Inner::RemoteClosed(remote_closed)
				}
				Either::Right(None) => Inner::Killed,
			},
			Inner::ConnectingLocalClosed(connecting_local_closed) => {
				match connecting_local_closed.poll(executor) {
					Either::Left(connecting_local_closed) => {
						Inner::ConnectingLocalClosed(connecting_local_closed)
					}
					Either::Right(Some(Either::Left(connected))) => Inner::LocalClosed(connected),
					Either::Right(Some(Either::Right(Either::Left(remote_closed)))) => {
						Inner::Closing(remote_closed)
					}
					Either::Right(Some(Either::Right(Either::Right(())))) => Inner::Closed,
					Either::Right(None) => Inner::Killed,
				}
			}
			Inner::Connected(connecting) => match connecting.poll(executor) {
				Either::Left(connected) => Inner::Connected(connected),
				Either::Right(Some(remote_closed)) => Inner::RemoteClosed(remote_closed),
				Either::Right(None) => Inner::Killed,
			},
			Inner::RemoteClosed(remote_closed) => match remote_closed.poll(executor) {
				Some(remote_closed) => Inner::RemoteClosed(remote_closed),
				None => Inner::Killed,
			},
			Inner::LocalClosed(local_closed) => match local_closed.poll(executor) {
				Either::Left(local_closed) => Inner::LocalClosed(local_closed),
				Either::Right(Some(Either::Left(closing))) => Inner::Closing(closing),
				Either::Right(Some(Either::Right(()))) => Inner::Closed,
				Either::Right(None) => Inner::Killed,
			},
			Inner::Closing(closing) => match closing.poll(executor) {
				Either::Left(closing) => Inner::Closing(closing),
				Either::Right(Some(())) => Inner::Closed,
				Either::Right(None) => Inner::Killed,
			},
			Inner::Closed => Inner::Closed,
			Inner::Killed => Inner::Killed,
			Inner::Xxx => unreachable!(),
		};
		if let &mut Inner::RemoteClosed(_) = self {
			self.close(executor);
		}
	}

	pub fn connecting(&self) -> bool {
		match self {
			&Inner::Connecting(_) => true,
			&Inner::ConnectingLocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn recvable(&self) -> bool {
		match self {
			&Inner::Connected(_) => true,
			&Inner::LocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn recv_avail<T: serde::de::DeserializeOwned + 'static>(
		&mut self, executor: &EpollExecutorContext,
	) -> bool {
		self.poll(executor);
		match self {
			&mut Inner::Connected(ref mut connected) => connected.recv_avail::<T>(executor),
			&mut Inner::LocalClosed(ref mut local_closed) => local_closed.recv_avail::<T>(executor),
			_ => false,
		}
	}

	pub fn recv<T: serde::de::DeserializeOwned + 'static>(
		&mut self, executor: &EpollExecutorContext,
	) -> T {
		let ret = match self {
			&mut Inner::Connected(ref mut connected) => connected.recv(executor),
			&mut Inner::LocalClosed(ref mut local_closed) => local_closed.recv(executor),
			_ => panic!(),
		};
		self.poll(executor);
		ret
	}

	pub fn sendable(&self) -> bool {
		match self {
			&Inner::Connected(_) => true,
			&Inner::RemoteClosed(_) => true,
			_ => false,
		}
	}

	pub fn send_avail(&self) -> bool {
		match self {
			&Inner::Connected(ref connected) => connected.send_avail(),
			&Inner::RemoteClosed(ref remote_closed) => remote_closed.send_avail(),
			_ => false,
		}
	}

	pub fn send<T: serde::ser::Serialize + 'static>(
		&mut self, x: T, executor: &EpollExecutorContext,
	) {
		match self {
			&mut Inner::Connected(ref mut connected) => connected.send(x, executor),
			&mut Inner::RemoteClosed(ref mut remote_closed) => remote_closed.send(x, executor),
			_ => panic!(),
		}
		self.poll(executor);
	}

	pub fn closable(&self) -> bool {
		match self {
			&Inner::Connecting(_) => true,
			&Inner::Connected(_) => true,
			&Inner::RemoteClosed(_) => true,
			_ => false,
		}
	}

	pub fn closed(&self) -> bool {
		match self {
			&Inner::Closed => true,
			_ => false,
		}
	}

	pub fn valid(&self) -> bool {
		match self {
			&Inner::Connecting(_) => true,
			&Inner::ConnectingLocalClosed(_) => true,
			&Inner::Connected(_) => true,
			&Inner::RemoteClosed(_) => true,
			&Inner::LocalClosed(_) => true,
			&Inner::Closing(_) => true,
			&Inner::Closed => true,
			&Inner::Killed => false,
			&Inner::Xxx => unreachable!(),
		}
	}

	pub fn close(&mut self, executor: &EpollExecutorContext) {
		*self = match mem::replace(self, Inner::Xxx) {
			Inner::Connecting(connecting) => match connecting.close(executor) {
				Either::Left(connecting_local_closed) => {
					Inner::ConnectingLocalClosed(connecting_local_closed)
				}
				Either::Right(Some(Either::Left(local_closed))) => Inner::LocalClosed(local_closed),
				Either::Right(Some(Either::Right(Either::Left(closing)))) => {
					Inner::Closing(closing)
				}
				Either::Right(Some(Either::Right(Either::Right(())))) => Inner::Closed,
				Either::Right(None) => Inner::Killed,
			},
			Inner::Connected(connected) => match connected.close(executor) {
				Either::Left(local_closed) => Inner::LocalClosed(local_closed),
				Either::Right(Some(Either::Left(closing))) => Inner::Closing(closing),
				Either::Right(Some(Either::Right(()))) => Inner::Closed,
				Either::Right(None) => Inner::Killed,
			},
			Inner::RemoteClosed(remote_closed) => match remote_closed.close(executor) {
				Either::Left(closing) => Inner::Closing(closing),
				Either::Right(Some(())) => Inner::Closed,
				Either::Right(None) => Inner::Killed,
			},
			Inner::ConnectingLocalClosed(_connecting_local_closed) => panic!(),
			Inner::LocalClosed(_local_closed) => panic!(),
			Inner::Closing(_closing) => panic!(),
			Inner::Closed => panic!(),
			Inner::Killed => panic!(),
			Inner::Xxx => unreachable!(),
		};
	}
	// pub fn drop(self, executor: &EpollExecutorContext) {
}

fn ord(a: &net::SocketAddr, b: &net::SocketAddr) -> bool {
	// TODO: cipher?
	assert_ne!(a, b);
	if a.ip() > b.ip() {
		return true;
	}
	if a.ip() < b.ip() {
		return false;
	}
	if a.port() > b.port() {
		return true;
	}
	if a.port() < b.port() {
		return false;
	}
	unreachable!()
}

#[derive(Debug)]
struct InnerConnecting {
	outgoing: Option<LinuxTcp>,
	incoming: Option<LinuxTcp>,
}
impl InnerConnecting {
	fn new(
		local: net::SocketAddr, remote: net::SocketAddr, incoming: Option<LinuxTcp>,
		executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<InnerConnected, InnerConnectedRemoteClosed>>> {
		if ord(&local, &remote) {
			InnerConnecting {
				outgoing: Some(LinuxTcp::connect(local, remote, executor)),
				incoming,
			}
		} else {
			InnerConnecting {
				outgoing: None,
				incoming,
			}
		}.poll(executor)
	}

	fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<InnerConnected, InnerConnectedRemoteClosed>>> {
		if self.incoming.is_some() {
			self.incoming.as_mut().unwrap().poll(executor);
			if !self.incoming.as_ref().unwrap().connecting() {
				return Either::Right(match InnerConnected::new(
					self.incoming.take().unwrap(),
					executor,
				) {
					Either::Left(connected) => Some(Either::Left(connected)),
					Either::Right(Some(remote_closed)) => Some(Either::Right(remote_closed)),
					Either::Right(None) => None,
				});
			}
		}
		if self.outgoing.is_some() {
			self.outgoing.as_mut().unwrap().poll(executor);
			if !self.outgoing.as_ref().unwrap().connecting() {
				return Either::Right(match InnerConnected::new(
					self.outgoing.take().unwrap(),
					executor,
				) {
					Either::Left(connected) => Some(Either::Left(connected)),
					Either::Right(Some(remote_closed)) => Some(Either::Right(remote_closed)),
					Either::Right(None) => None,
				});
			}
		}
		Either::Left(self)
	}

	fn close(
		self, executor: &EpollExecutorContext,
	) -> Either<
		InnerConnectingLocalClosed,
		Option<Either<InnerConnectedLocalClosed, Either<InnerConnectedClosing, ()>>>,
	> {
		InnerConnectingLocalClosed::new(self.outgoing, self.incoming, executor)
	}
}

#[derive(Debug)]
struct InnerConnectingLocalClosed {
	outgoing: Option<LinuxTcp>,
	incoming: Option<LinuxTcp>,
}
impl InnerConnectingLocalClosed {
	fn new(
		outgoing: Option<LinuxTcp>, incoming: Option<LinuxTcp>, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<InnerConnectedLocalClosed, Either<InnerConnectedClosing, ()>>>>
	{
		InnerConnectingLocalClosed { outgoing, incoming }.poll(executor)
	}

	fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<InnerConnectedLocalClosed, Either<InnerConnectedClosing, ()>>>>
	{
		if self.incoming.is_some() {
			self.incoming.as_mut().unwrap().poll(executor);
			if self.incoming.as_ref().unwrap().closable() {
				self.incoming.as_mut().unwrap().close(executor);
			}
			if !self.incoming.as_ref().unwrap().connecting() {
				return Either::Right(match InnerConnectedLocalClosed::new(
					self.incoming.take().unwrap(),
					serialize::Serializer::new(),
					serialize::Deserializer::new(),
					false,
					executor,
				) {
					Either::Left(connected) => Some(Either::Left(connected)),
					Either::Right(Some(remote_closed)) => Some(Either::Right(remote_closed)),
					Either::Right(None) => None,
				});
			}
		}
		if self.outgoing.is_some() {
			self.outgoing.as_mut().unwrap().poll(executor);
			if self.outgoing.as_ref().unwrap().closable() {
				self.outgoing.as_mut().unwrap().close(executor);
			}
			if !self.outgoing.as_ref().unwrap().connecting() {
				return Either::Right(match InnerConnectedLocalClosed::new(
					self.outgoing.take().unwrap(),
					serialize::Serializer::new(),
					serialize::Deserializer::new(),
					false,
					executor,
				) {
					Either::Left(connected) => Some(Either::Left(connected)),
					Either::Right(Some(remote_closed)) => Some(Either::Right(remote_closed)),
					Either::Right(None) => None,
				});
			}
		}
		if self.incoming.is_none() && self.outgoing.is_none() {
			return Either::Right(Some(Either::Right(Either::Right(()))));
		}
		Either::Left(self)
	}
}

#[derive(Debug)]
struct InnerConnected {
	connection: LinuxTcp,
	send_serializer: serialize::Serializer,
	recv_deserializer: serialize::Deserializer,
	recv_deserializer_given: bool,
}
impl InnerConnected {
	fn new(
		connection: LinuxTcp, executor: &EpollExecutorContext,
	) -> Either<Self, Option<InnerConnectedRemoteClosed>> {
		InnerConnected {
			connection,
			send_serializer: serialize::Serializer::new(),
			recv_deserializer: serialize::Deserializer::new(),
			recv_deserializer_given: false,
		}.poll(executor)
	}

	fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<InnerConnectedRemoteClosed>> {
		assert!(!self.connection.connecting());
		let mut progress = true;
		loop {
			if self.connection.sendable() {
				while self.connection.send_avail() && self.send_serializer.pull_avail() {
					self.connection.send(self.send_serializer.pull(), executor);
					progress = true;
				}
			}
			if self.connection.recvable() {
				while self.connection.recv_avail() && self.recv_deserializer.push_avail() {
					self.recv_deserializer.push(self.connection.recv(executor));
					progress = true;
				}
			}
			if !progress {
				break;
			}
			progress = false;
			self.connection.poll(executor);
		}
		// if !self.connection.recvable() {
		// 	assert!(!self.recv_deserializer.push_avail(), "{}: CLOSED WHILE PENDING RECV", ::internal::pid());
		// }
		if !self.connection.recvable()
			&& !self.recv_deserializer.mid()
			&& !self.recv_deserializer.pull2_avail()
		{
			// self.recv_deserializer.pull_avail() {
			// assert!(!self.recv_deserializer_given);
			return Either::Right(match InnerConnectedRemoteClosed::new(
				self.connection,
				self.send_serializer,
				executor,
			) {
				Some(remote_closed) => Some(remote_closed),
				None => None,
			});
		}
		if !self.connection.valid() {
			return Either::Right(None);
		}
		Either::Left(self)
	}

	fn send_avail(&self) -> bool {
		self.send_serializer.push_avail()
	}

	fn send<T: serde::ser::Serialize + 'static>(&mut self, t: T, _executor: &EpollExecutorContext) {
		self.send_serializer.push(t);
		// self.poll(executor);
	}

	fn recv_avail<T: serde::de::DeserializeOwned + 'static>(
		&mut self, executor: &EpollExecutorContext,
	) -> bool {
		// logln!("{:?}:{:?} recv_avail {:?}", 0000, self as *mut InnerConnected , unsafe{intrinsics::type_name::<T>()});
		if !self.recv_deserializer_given {
			// logln!("{:?}:{:?} recv_deserializer_given {:?}", 0000, self as *mut InnerConnected , unsafe{intrinsics::type_name::<T>()});
			self.recv_deserializer.pull::<T>();
			self.recv_deserializer_given = true;

			let mut progress = true;
			loop {
				if self.connection.recvable() {
					while self.connection.recv_avail() && self.recv_deserializer.push_avail() {
						self.recv_deserializer.push(self.connection.recv(executor));
						progress = true;
					}
				}
				if !progress {
					break;
				}
				progress = false;
				self.connection.poll(executor);
			}
		}
		let ret = self.recv_deserializer.pull2_avail();
		ret
	}

	fn recv<T: serde::de::DeserializeOwned + 'static>(
		&mut self, _executor: &EpollExecutorContext,
	) -> T {
		// logln!("{:?}:{:?} recv {:?}", 0000, self as *mut InnerConnected , unsafe{intrinsics::type_name::<T>()});
		self.recv_deserializer_given = false;
		let ret = self.recv_deserializer.pull2::<T>();
		// self.poll(executor);
		ret
	}

	fn close(
		self, executor: &EpollExecutorContext,
	) -> Either<InnerConnectedLocalClosed, Option<Either<InnerConnectedClosing, ()>>> {
		InnerConnectedLocalClosed::new(
			self.connection,
			self.send_serializer,
			self.recv_deserializer,
			self.recv_deserializer_given,
			executor,
		)
	}
}

#[derive(Debug)]
struct InnerConnectedRemoteClosed {
	connection: LinuxTcp,
	send_serializer: serialize::Serializer,
}
impl InnerConnectedRemoteClosed {
	fn new(
		connection: LinuxTcp, send_serializer: serialize::Serializer,
		executor: &EpollExecutorContext,
	) -> Option<Self> {
		InnerConnectedRemoteClosed {
			connection,
			send_serializer,
		}.poll(executor)
	}

	fn poll(mut self, executor: &EpollExecutorContext) -> Option<Self> {
		assert!(!self.connection.recvable());
		let mut progress = true;
		loop {
			if self.connection.sendable() {
				while self.connection.send_avail() && self.send_serializer.pull_avail() {
					self.connection.send(self.send_serializer.pull(), executor);
					progress = true;
				}
			}
			if !progress {
				break;
			}
			progress = false;
			self.connection.poll(executor);
		}
		if !self.connection.valid() {
			return None;
		}
		Some(self)
	}

	fn send_avail(&self) -> bool {
		self.send_serializer.push_avail()
	}

	fn send<T: serde::ser::Serialize + 'static>(&mut self, t: T, _executor: &EpollExecutorContext) {
		self.send_serializer.push(t);
		// self.poll(executor);
	}

	fn close(self, executor: &EpollExecutorContext) -> Either<InnerConnectedClosing, Option<()>> {
		InnerConnectedClosing::new(self.connection, self.send_serializer, executor)
	}
}

#[derive(Debug)]
struct InnerConnectedLocalClosed {
	connection: LinuxTcp,
	send_serializer: serialize::Serializer,
	recv_deserializer: serialize::Deserializer,
	recv_deserializer_given: bool,
}
impl InnerConnectedLocalClosed {
	fn new(
		connection: LinuxTcp, send_serializer: serialize::Serializer,
		recv_deserializer: serialize::Deserializer, recv_deserializer_given: bool,
		executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<InnerConnectedClosing, ()>>> {
		InnerConnectedLocalClosed {
			connection,
			send_serializer,
			recv_deserializer,
			recv_deserializer_given,
		}.poll(executor)
	}

	fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<InnerConnectedClosing, ()>>> {
		assert!(!self.connection.connecting());
		let mut progress = true;
		loop {
			if self.connection.sendable() {
				while self.connection.send_avail() && self.send_serializer.pull_avail() {
					self.connection.send(self.send_serializer.pull(), executor);
					progress = true;
				}
			}
			if self.connection.recvable() {
				while self.connection.recv_avail() && self.recv_deserializer.push_avail() {
					self.recv_deserializer.push(self.connection.recv(executor));
					progress = true;
				}
			}
			if !progress {
				break;
			}
			progress = false;
			self.connection.poll(executor);
		}
		if self.connection.sendable() && !self.send_serializer.pull_avail() {
			self.connection.close(executor);
		}
		// if !self.connection.recvable() {
		// 	assert!(!self.recv_deserializer.push_avail(), "{}: CLOSED WHILE PENDING RECV", ::internal::pid());
		// }
		if !self.connection.recvable()
			&& !self.recv_deserializer.mid()
			&& !self.recv_deserializer.pull2_avail()
		{
			// self.recv_deserializer.pull_avail() {
			// assert!(!self.recv_deserializer_given);
			return Either::Right(match InnerConnectedClosing::new(
				self.connection,
				self.send_serializer,
				executor,
			) {
				Either::Left(closing) => Some(Either::Left(closing)),
				Either::Right(Some(())) => Some(Either::Right(())),
				Either::Right(None) => None,
			});
		}
		if !self.connection.valid() {
			return Either::Right(None);
		}
		Either::Left(self)
	}

	fn recv_avail<T: serde::de::DeserializeOwned + 'static>(
		&mut self, executor: &EpollExecutorContext,
	) -> bool {
		// logln!("{:?}:{:?} recv_avail {:?}", 0000, self as *mut InnerConnectedLocalClosed, unsafe{intrinsics::type_name::<T>()});
		if !self.recv_deserializer_given {
			// logln!("{:?}:{:?} recv_deserializer_given {:?}", 0000, self as *mut InnerConnectedLocalClosed, unsafe{intrinsics::type_name::<T>()});
			self.recv_deserializer.pull::<T>();
			self.recv_deserializer_given = true;

			let mut progress = true;
			loop {
				if self.connection.recvable() {
					while self.connection.recv_avail() && self.recv_deserializer.push_avail() {
						self.recv_deserializer.push(self.connection.recv(executor));
						progress = true;
					}
				}
				if !progress {
					break;
				}
				progress = false;
				self.connection.poll(executor);
			}
		}
		let ret = self.recv_deserializer.pull2_avail();
		ret
	}

	fn recv<T: serde::de::DeserializeOwned + 'static>(
		&mut self, _executor: &EpollExecutorContext,
	) -> T {
		// logln!("{:?}:{:?} recv {:?}", 0000, self as *mut InnerConnectedLocalClosed, unsafe{intrinsics::type_name::<T>()});
		self.recv_deserializer_given = false;
		let ret = self.recv_deserializer.pull2::<T>();
		// self.poll(executor);
		ret
	}
}

#[derive(Debug)]
struct InnerConnectedClosing {
	connection: LinuxTcp,
	send_serializer: serialize::Serializer,
}
impl InnerConnectedClosing {
	fn new(
		connection: LinuxTcp, send_serializer: serialize::Serializer,
		executor: &EpollExecutorContext,
	) -> Either<Self, Option<()>> {
		InnerConnectedClosing {
			connection,
			send_serializer,
		}.poll(executor)
	}

	fn poll(mut self, executor: &EpollExecutorContext) -> Either<Self, Option<()>> {
		assert!(!self.connection.recvable());
		let mut progress = true;
		loop {
			if self.connection.sendable() {
				while self.connection.send_avail() && self.send_serializer.pull_avail() {
					self.connection.send(self.send_serializer.pull(), executor);
					progress = true;
				}
			}
			if !progress {
				break;
			}
			progress = false;
			self.connection.poll(executor);
		}
		if self.connection.sendable() && !self.send_serializer.pull_avail() {
			self.connection.close(executor);
		}
		if !self.connection.valid() {
			return Either::Right(None);
		}
		if self.connection.closed() {
			return Either::Right(Some(()));
		}
		Either::Left(self)
	}
}

/// Channel operation error modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelError {
	/// The remote process has exited, thus `send()`/`recv()` could never succeed.
	Exited,
	/// The remote process terminated abruptly, or the channel was killed by the OS or hardware.
	Error,
}
impl fmt::Display for ChannelError {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			ChannelError::Error => {
				write!(f, "Remote process died or channel killed by OS/hardware")
			} //(ref err) => err.fmt(f),
			ChannelError::Exited => write!(f, "Remote process already exited"),
		}
	}
}
impl error::Error for ChannelError {
	fn description(&self) -> &str {
		match *self {
			ChannelError::Error => "remote process died or channel killed by OS/hardware", //(ref err) => err.description(),
			ChannelError::Exited => "not found",
		}
	}

	fn cause(&self) -> Option<&error::Error> {
		match *self {
			ChannelError::Error => None, //(ref err) => Some(err),
			ChannelError::Exited => None,
		}
	}
}

pub struct Sender<T: serde::ser::Serialize> {
	channel: Option<Arc<sync::RwLock<Option<Channel>>>>,
	_marker: marker::PhantomData<fn(T)>,
}
impl<T: serde::ser::Serialize> Sender<T> {
	pub fn new(remote: net::SocketAddr, context: &Context) -> Option<Sender<T>> {
		let (executor, sockets, local) = (&context.executor, &context.sockets, &context.local);
		let sockets = &mut *sockets.write().unwrap();
		let channel = if !sockets.contains_key(&remote) {
			let channel = Arc::new(sync::RwLock::new(None));
			let executor_key: *const sync::RwLock<Option<Channel>> = &*channel;
			let executor = &executor.context(executor_key as *const ());
			let mut inner = Channel::new(Inner::connect(local.clone(), remote, None, executor));
			inner.senders_count += 1;
			*channel.try_write().unwrap() = Some(inner);
			let x = sockets.insert(remote, channel.clone());
			assert!(x.is_none());
			logln!(
				"{:?}:{:?} new sender {:?}",
				getpid(),
				gettid(),
				executor_key
			);
			channel
		} else {
			let channel = sockets.get_mut(&remote).unwrap();
			if channel.write().unwrap().as_mut().unwrap().senders_count > 0 {
				return None;
			}
			channel.write().unwrap().as_mut().unwrap().senders_count += 1;
			let executor_key: *const sync::RwLock<Option<Channel>> = &**channel;
			logln!(
				"{:?}:{:?} retain sender {:?}",
				getpid(),
				gettid(),
				executor_key
			);
			channel.clone()
		};
		assert_eq!(
			sync::Arc::strong_count(&channel),
			1 + {
				let channel = channel.read().unwrap();
				channel.as_ref().unwrap().senders_count + channel.as_ref().unwrap().receivers_count
			}
		);
		Some(Sender {
			channel: Some(channel),
			_marker: marker::PhantomData,
		})
	}

	fn async_send_available(&self, _context: &Context) -> bool {
		match &self
			.channel
			.as_ref()
			.unwrap()
			.read()
			.unwrap()
			.as_ref()
			.unwrap()
			.inner
		{
			&Inner::Connected(ref connected) => connected.send_avail(),
			&Inner::RemoteClosed(ref connected) => connected.send_avail(),
			&Inner::Killed => true,
			&Inner::Closed => true,
			_ => false,
		}
	}

	fn async_send(&self, t: T, context: &Context) -> Result<(), ChannelError>
	where
		T: 'static,
	{
		let executor = &context.executor;
		let executor_key: *const sync::RwLock<Option<Channel>> = &**self.channel.as_ref().unwrap();
		let executor = &executor.context(executor_key as *const ());
		let mut channel = self.channel.as_ref().unwrap().write().unwrap();
		// assert_eq!(sync::Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
		let inner = &mut channel.as_mut().unwrap().inner;
		if !inner.valid() {
			return Err(ChannelError::Error);
		}
		if !inner.sendable() {
			return Err(ChannelError::Exited);
		}
		inner.send(t, executor);
		Ok(())
		// TODO: unpark queue?
	}

	pub fn xxx_send<'a>(
		&'a self, context: &Context,
	) -> Option<impl FnOnce(T, &Context) -> Result<(), ChannelError> + 'a>
	where
		T: 'static,
	{
		if self.async_send_available(context) {
			Some(move |t, context: &Context| self.async_send(t, context))
		} else {
			None
		}
	}

	pub fn send<F: FnMut() -> C, C: ops::Deref<Target = Context>>(
		&self, t: T, context: &mut F,
	) -> Result<(), ChannelError>
	where
		T: 'static,
	{
		let x = cell::RefCell::new(None);
		select(
			vec![Box::new(self.selectable_send(
				|| {
					*x.borrow_mut() = Some(Ok(()));
					t
				},
				|e| *x.borrow_mut() = Some(Err(e)),
			))],
			context,
		);
		x.into_inner().unwrap()
	}

	pub fn selectable_send<'a, F: FnOnce() -> T + 'a, E: FnOnce(ChannelError) + 'a>(
		&'a self, f: F, e: E,
	) -> impl Selectable + 'a
	where
		T: 'static,
	{
		Send(self, Some((f, e)))
	}

	pub fn drop(mut self, context: &Context) {
		let mut sockets = context.sockets.write().unwrap();
		let channel_arc = self.channel.take().unwrap();
		mem::forget(self);
		let executor_key: *const sync::RwLock<Option<Channel>> = &*channel_arc;
		let mut channel = channel_arc.write().unwrap();
		assert_eq!(
			sync::Arc::strong_count(&channel_arc),
			1 + channel.as_ref().unwrap().senders_count + channel.as_ref().unwrap().receivers_count,
			"{:?}:{:?} {:?}",
			getpid(),
			gettid(),
			executor_key
		);
		if {
			let channel = channel.as_mut().unwrap();
			channel.senders_count -= 1;
			assert_eq!(channel.senders_count, 0);
			logln!(
				"{:?}:{:?} release sender {:?}",
				getpid(),
				gettid(),
				executor_key
			);
			channel.senders_count == 0 && channel.receivers_count == 0 && channel.inner.closed()
		} {
			let x = channel.take().unwrap();
			assert!(x.senders_count == 0 && x.receivers_count == 0 && x.inner.closed());
			let key = sockets
				.iter()
				.find(|&(_key, channel)| {
					let executor_key2: *const sync::RwLock<Option<Channel>> = &**channel;
					executor_key2 == executor_key
				})
				.unwrap()
				.0
				.clone();
			mem::drop(channel);
			assert_eq!(sync::Arc::strong_count(&channel_arc), 2);
			mem::drop(channel_arc);
			logln!(
				"{:?}:{:?} drop sender {:?}",
				getpid(),
				gettid(),
				executor_key
			);
			let mut x =
				Arc::try_unwrap(sockets.remove(&key).unwrap()).expect("channel.try_unwrap drop 1");
			assert!(x.get_mut().unwrap().is_none());
			logln!("channel.try_unwrap drop 1 success");
		}
	}

	pub fn futures_poll_ready(
		&mut self, cx: &mut futures::task::Context, context: &Context,
	) -> Result<futures::Async<()>, ChannelError>
	where
		T: 'static,
	{
		if let Some(_recv) = self.xxx_send(context) {
			Ok(futures::Async::Ready(()))
		} else {
			self.channel
				.as_ref()
				.unwrap()
				.write()
				.unwrap()
				.as_mut()
				.unwrap()
				.senders_futures
				.push(cx.waker().clone());
			Ok(futures::Async::Pending)
		}
	}

	pub fn futures_start_send(&mut self, item: T, context: &Context) -> Result<(), ChannelError>
	where
		T: 'static,
	{
		self.xxx_send(context).expect(
			"called futures::Sink::start_send without the go-ahead from futures::Sink::poll_ready",
		)(item, context)
	}
}
impl<T: serde::ser::Serialize> ops::Drop for Sender<T> {
	fn drop(&mut self) {
		panic!("call .drop(context) rather than this");
	}
}
struct Send<'a, T: serde::ser::Serialize + 'static, F: FnOnce() -> T, E: FnOnce(ChannelError)>(
	&'a Sender<T>,
	Option<(F, E)>,
);
impl<'a, T: serde::ser::Serialize + 'static, F: FnOnce() -> T, E: FnOnce(ChannelError)> fmt::Debug
	for Send<'a, T, F, E>
{
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Send").field("sender", &self.0).finish()
	}
}
impl<'a, T: serde::ser::Serialize + 'static, F: FnOnce() -> T, E: FnOnce(ChannelError)> Selectable
	for Send<'a, T, F, E>
{
	fn subscribe(&self, thread: thread::Thread) {
		let x = self
			.0
			.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.senders
			.insert(thread.id(), thread);
		assert!(x.is_none());
	}

	fn available(&self, context: &Context) -> bool {
		self.0.async_send_available(context)
	}

	fn run(&mut self, context: &Context) {
		let (f, e) = self.1.take().unwrap();
		let t = f();
		match self.0.async_send(t, context) {
			Ok(()) => (),
			Err(t) => e(t),
		}
	}

	fn unsubscribe(&self, thread: thread::Thread) {
		self.0
			.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.senders
			.remove(&thread.id())
			.unwrap();
	}
}
impl<T: serde::ser::Serialize> fmt::Debug for Sender<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Sender")
			.field("inner", &self.channel)
			.finish()
	}
}

pub struct Receiver<T: serde::de::DeserializeOwned> {
	channel: Option<Arc<sync::RwLock<Option<Channel>>>>,
	_marker: marker::PhantomData<fn() -> T>,
}
impl<T: serde::de::DeserializeOwned> Receiver<T> {
	pub fn new(remote: net::SocketAddr, context: &Context) -> Option<Receiver<T>> {
		let (executor, sockets, local) = (&context.executor, &context.sockets, &context.local);
		let sockets = &mut *sockets.write().unwrap();
		let channel = if !sockets.contains_key(&remote) {
			let channel = Arc::new(sync::RwLock::new(None));
			let executor_key: *const sync::RwLock<Option<Channel>> = &*channel;
			let executor = &executor.context(executor_key as *const ());
			let mut inner = Channel::new(Inner::connect(local.clone(), remote, None, executor));
			inner.receivers_count += 1;
			*channel.try_write().unwrap() = Some(inner);
			let x = sockets.insert(remote, channel.clone());
			assert!(x.is_none());
			logln!(
				"{:?}:{:?} new receiver {:?}",
				getpid(),
				gettid(),
				executor_key
			);
			channel
		} else {
			let channel = sockets.get_mut(&remote).unwrap();
			if channel.write().unwrap().as_mut().unwrap().receivers_count > 0 {
				return None;
			}
			channel.write().unwrap().as_mut().unwrap().receivers_count += 1;
			let executor_key: *const sync::RwLock<Option<Channel>> = &**channel;
			logln!(
				"{:?}:{:?} retain receiver {:?}",
				getpid(),
				gettid(),
				executor_key
			);
			channel.clone()
		};
		assert_eq!(
			sync::Arc::strong_count(&channel),
			1 + {
				let channel = channel.read().unwrap();
				channel.as_ref().unwrap().senders_count + channel.as_ref().unwrap().receivers_count
			}
		);
		Some(Receiver {
			channel: Some(channel),
			_marker: marker::PhantomData,
		})
	}

	fn async_recv_available(&self, context: &Context) -> bool
	where
		T: 'static,
	{
		let executor = &context.executor;
		let executor_key: *const sync::RwLock<Option<Channel>> = &**self.channel.as_ref().unwrap();
		let executor = &executor.context(executor_key as *const ());
		let mut channel = self.channel.as_ref().unwrap().write().unwrap();
		// assert_eq!(sync::Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
		let inner = &mut channel.as_mut().unwrap().inner;
		// inner.recv_avail::<T>(executor)
		match inner {
			&mut Inner::Connected(ref mut connected) => {
				// assert!(connected.connection.recvable());
				connected.recv_avail::<T>(executor)
			}
			&mut Inner::LocalClosed(ref mut connected) => {
				// assert!(connected.connection.recvable());
				connected.recv_avail::<T>(executor)
			}
			&mut Inner::Killed => true,
			&mut Inner::Closed => true,
			_ => false,
		}
	}

	fn async_recv(&self, context: &Context) -> Result<T, ChannelError>
	where
		T: 'static,
	{
		let executor = &context.executor;
		let executor_key: *const sync::RwLock<Option<Channel>> = &**self.channel.as_ref().unwrap();
		let executor = &executor.context(executor_key as *const ());
		let mut channel = self.channel.as_ref().unwrap().write().unwrap();
		// assert_eq!(sync::Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
		let inner = &mut channel.as_mut().unwrap().inner;
		if !inner.valid() {
			return Err(ChannelError::Error);
		}
		if !inner.recvable() {
			return Err(ChannelError::Exited);
		}
		Ok(inner.recv(executor))
		// TODO: unpark queue?
	}

	pub fn xxx_recv<'a>(
		&'a self, context: &Context,
	) -> Option<impl FnOnce(&Context) -> Result<T, ChannelError> + 'a>
	where
		T: 'static,
	{
		if self.async_recv_available(context) {
			Some(move |context: &Context| self.async_recv(context))
		} else {
			None
		}
	}

	pub fn recv<F: FnMut() -> C, C: ops::Deref<Target = Context>>(
		&self, context: &mut F,
	) -> Result<T, ChannelError>
	where
		T: 'static,
	{
		let x = cell::RefCell::new(None);
		select(
			vec![Box::new(self.selectable_recv(
				|t| *x.borrow_mut() = Some(Ok(t)),
				|e| *x.borrow_mut() = Some(Err(e)),
			))],
			context,
		);
		x.into_inner().unwrap()
	}

	pub fn selectable_recv<'a, F: FnOnce(T) + 'a, E: FnOnce(ChannelError) + 'a>(
		&'a self, f: F, e: E,
	) -> impl Selectable + 'a
	where
		T: 'static,
	{
		Recv(self, Some((f, e)))
	}

	pub fn drop(mut self, context: &Context) {
		let mut sockets = context.sockets.write().unwrap();
		let channel_arc = self.channel.take().unwrap();
		mem::forget(self);
		let executor_key: *const sync::RwLock<Option<Channel>> = &*channel_arc;
		let mut channel = channel_arc.write().unwrap();
		assert_eq!(
			sync::Arc::strong_count(&channel_arc),
			1 + channel.as_ref().unwrap().senders_count + channel.as_ref().unwrap().receivers_count,
			"{:?}:{:?} {:?}",
			getpid(),
			gettid(),
			executor_key
		);
		if {
			let channel = channel.as_mut().unwrap();
			channel.receivers_count -= 1;
			assert_eq!(channel.receivers_count, 0);
			logln!(
				"{:?}:{:?} release receiver {:?}",
				getpid(),
				gettid(),
				executor_key
			);
			channel.senders_count == 0 && channel.receivers_count == 0 && channel.inner.closed()
		} {
			let x = channel.take().unwrap();
			assert!(x.senders_count == 0 && x.receivers_count == 0 && x.inner.closed());
			let key = sockets
				.iter()
				.find(|&(_key, channel)| {
					let executor_key2: *const sync::RwLock<Option<Channel>> = &**channel;
					executor_key2 == executor_key
				})
				.unwrap()
				.0
				.clone();
			mem::drop(channel);
			assert_eq!(sync::Arc::strong_count(&channel_arc), 2);
			mem::drop(channel_arc);
			logln!(
				"{:?}:{:?} drop receiver {:?}",
				getpid(),
				gettid(),
				executor_key
			);
			let mut x =
				Arc::try_unwrap(sockets.remove(&key).unwrap()).expect("channel.try_unwrap drop 2");
			assert!(x.get_mut().unwrap().is_none());
			logln!("channel.try_unwrap drop 2 success");
		}
	}

	pub fn futures_poll_next(
		&mut self, cx: &mut futures::task::Context, context: &Context,
	) -> Result<futures::Async<Option<T>>, ChannelError>
	where
		T: 'static,
	{
		if let Some(recv) = self.xxx_recv(context) {
			match recv(context) {
				Ok(t) => Ok(futures::Async::Ready(Some(t))),
				Err(err) => Err(err),
			}
		} else {
			self.channel
				.as_ref()
				.unwrap()
				.write()
				.unwrap()
				.as_mut()
				.unwrap()
				.receivers_futures
				.push(cx.waker().clone());
			Ok(futures::Async::Pending)
		}
	}
}
impl<T: serde::de::DeserializeOwned> ops::Drop for Receiver<T> {
	fn drop(&mut self) {
		panic!("call .drop(context) rather than this");
	}
}
struct Recv<'a, T: serde::de::DeserializeOwned + 'static, F: FnOnce(T), E: FnOnce(ChannelError)>(
	&'a Receiver<T>,
	Option<(F, E)>,
);
impl<'a, T: serde::de::DeserializeOwned + 'static, F: FnOnce(T), E: FnOnce(ChannelError)> fmt::Debug
	for Recv<'a, T, F, E>
{
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Recv").field("receiver", &self.0).finish()
	}
}
impl<'a, T: serde::de::DeserializeOwned + 'static, F: FnOnce(T), E: FnOnce(ChannelError)> Selectable
	for Recv<'a, T, F, E>
{
	fn subscribe(&self, thread: thread::Thread) {
		let x = self
			.0
			.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.receivers
			.insert(thread.id(), thread);
		assert!(x.is_none());
	}

	fn available(&self, context: &Context) -> bool {
		self.0.async_recv_available(context)
	}

	fn run(&mut self, context: &Context) {
		let (f, e) = self.1.take().unwrap();
		match self.0.async_recv(context) {
			Ok(t) => f(t),
			Err(t) => e(t),
		}
	}

	fn unsubscribe(&self, thread: thread::Thread) {
		self.0
			.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.receivers
			.remove(&thread.id())
			.unwrap();
	}
}
impl<T: serde::de::DeserializeOwned> fmt::Debug for Receiver<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Receiver")
			.field("inner", &self.channel)
			.finish()
	}
}

/// Types that can be [select()](select)ed upon.
///
/// [select()](select) lets you block on multiple blocking operations until progress can be made on at least one.
///
/// [Receiver::selectable_recv()](Receiver::selectable_recv) and [Sender::selectable_send()](Sender::selectable_send) let one create `Selectable` objects, any number of which can be passed to [select()](select). [select()](select) then blocks until at least one is progressable, and then from any that are progressable picks one at random and executes it.
///
/// It is inspired by the [select()](select) of go, which itself draws from David May's language [occam](https://en.wikipedia.org/wiki/Occam_(programming_language)) and Tony Hoare’s formalisation of [Communicating Sequential Processes](https://en.wikipedia.org/wiki/Communicating_sequential_processes).
pub trait Selectable: fmt::Debug {
	#[doc(hidden)]
	fn subscribe(&self, thread::Thread);
	#[doc(hidden)]
	fn available(&self, context: &Context) -> bool;
	#[doc(hidden)]
	fn run(&mut self, context: &Context);
	#[doc(hidden)]
	fn unsubscribe(&self, thread::Thread);
}
pub fn select<'a, F: FnMut() -> C, C: ops::Deref<Target = Context>>(
	select: Vec<Box<Selectable + 'a>>, context: &mut F,
) -> impl Iterator<Item = Box<Selectable + 'a>> + 'a {
	for selectable in select.iter() {
		selectable.subscribe(thread::current());
	}
	let ret = loop {
		let mut rand = Rand::new();
		for (i, selectable) in select.iter().enumerate() {
			if selectable.available(&*context()) {
				rand.push(i, &mut rand::thread_rng());
			}
		}
		if let Some(available) = rand.get() {
			break available;
		}
		thread::park();
	};
	for selectable in select.iter() {
		selectable.unsubscribe(thread::current());
	}
	let mut xxx = Vec::with_capacity(select.len() - 1);
	for (i, mut select) in select.into_iter().enumerate() {
		if i != ret {
			xxx.push(select);
		} else {
			select.run(&*context());
		}
	}
	xxx.into_iter()
}
struct Rand<T> {
	res: Option<T>,
	count: usize,
}
impl<T> Rand<T> {
	fn new() -> Rand<T> {
		Rand {
			res: None,
			count: 0,
		}
	}

	fn push<R: rand::Rng>(&mut self, x: T, rng: &mut R) {
		self.count += 1;
		if rng.gen_range(0, self.count) == 0 {
			self.res = Some(x);
		}
	}

	fn get(self) -> Option<T> {
		self.res
	}
}
