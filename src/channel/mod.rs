mod inner;
mod inner_states;

use either::Either;
use log::trace;
use nix::sys::socket;
use notifier::{Notifier, Triggerer};
use serde::{de::DeserializeOwned, Serialize};
use std::{
	borrow::Borrow, collections::{hash_map, HashMap}, error, fmt, marker, mem, net::{IpAddr, SocketAddr}, pin::Pin, ptr, sync::{mpsc, Arc, RwLock, RwLockWriteGuard}, task::{Context, Poll, Waker}, thread::{self, Thread, ThreadId}, time::{Duration, Instant}
};
use tcp_typed::{Connection, Listener};

use super::{Fd, Never};

pub use self::{inner::*, inner_states::*};
pub use tcp_typed::{socket_forwarder, SocketForwardee, SocketForwarder};

#[derive(Copy, Clone, PartialEq, Eq)]
struct Key(*const ());
/// Because `*const ()`. Pointers aren't really not Send+Sync, it's more of a lint.
unsafe impl marker::Send for Key {}
unsafe impl Sync for Key {}
impl From<usize> for Key {
	fn from(x: usize) -> Self {
		Self(x as *const ())
	}
}
impl From<Key> for usize {
	fn from(x: Key) -> Self {
		x.0 as Self
	}
}

pub struct Handle {
	triggerer: Option<Triggerer>,
	tcp_thread: Option<thread::JoinHandle<()>>,
}
impl Drop for Handle {
	fn drop(&mut self) {
		drop(self.triggerer.take().unwrap());
		self.tcp_thread.take().unwrap().join().unwrap();
	}
}
pub struct Reactor {
	notifier: Notifier<Key>,
	listener: RwLock<Option<Listener>>,
	sockets: RwLock<HashMap<SocketAddr, Arc<RwLock<Option<Channel>>>>>,
	local: SocketAddr,
}
impl Reactor {
	pub fn new(host: IpAddr) -> (Self, u16) {
		let notifier = Notifier::new();
		let (listener, port) = Listener::new_ephemeral(&host, &notifier.context(Key(ptr::null())));
		let sockets = RwLock::new(HashMap::new());
		let local = SocketAddr::new(host, port);
		(
			Self {
				notifier,
				listener: RwLock::new(Some(listener)),
				sockets,
				local,
			},
			port,
		)
	}

	pub fn with_fd(fd: Fd) -> Self {
		let notifier = Notifier::new();
		let listener = Listener::with_fd(fd, &notifier.context(Key(ptr::null())));
		let sockets = RwLock::new(HashMap::new());
		let local = if let socket::SockAddr::Inet(inet) = socket::getsockname(fd).unwrap() {
			inet.to_std()
		} else {
			panic!()
		};
		Self {
			notifier,
			listener: RwLock::new(Some(listener)),
			sockets,
			local,
		}
	}

	pub fn with_forwardee(socket_forwardee: SocketForwardee, local: SocketAddr) -> Self {
		let notifier = Notifier::new();
		let listener =
			Listener::with_socket_forwardee(socket_forwardee, &notifier.context(Key(ptr::null())));
		let sockets = RwLock::new(HashMap::new());
		Self {
			notifier,
			listener: RwLock::new(Some(listener)),
			sockets,
			local,
		}
	}

	pub fn run<
		F: FnMut() -> C + marker::Send + 'static,
		C: Borrow<Self>,
		F1: FnMut(&Fd) -> Option<SocketForwarder> + marker::Send + 'static,
	>(
		mut context: F, mut accept_hook: F1,
	) -> Handle {
		let (triggerer, triggeree) = {
			let context = context();
			context
				.borrow()
				.notifier
				.context(Key(1 as *const ()))
				.add_trigger()
		};
		let mut triggeree = Some(triggeree);
		let tcp_thread = thread::Builder::new()
			.name(String::from("tcp-thread"))
			.spawn(move || {
				let context = context();
				let context = context.borrow();
				let mut listener = context.listener.try_write().unwrap();
				let (notifier, listener, sockets, local) = (
					&context.notifier,
					listener.as_mut().unwrap(),
					&context.sockets,
					&context.local,
				);
				let mut done: Option<
					RwLockWriteGuard<HashMap<SocketAddr, Arc<RwLock<Option<Channel>>>>>,
				> = None;
				while done.is_none()
					|| done.as_ref().unwrap().iter().any(|(_, ref inner)| {
						// TODO: maintain count
						let inner = inner.read().unwrap();
						let inner = &inner.as_ref().unwrap().inner;
						inner.valid() && !inner.closed()
					}) {
					let mut sender = None;
					let mut catcher = None;
					if let Some(ref sockets) = done {
						struct Ptr<T: ?Sized>(T);
						unsafe impl<T: ?Sized> marker::Send for Ptr<T> {}
						unsafe impl<T: ?Sized> Sync for Ptr<T> {}
						let (sender_, receiver) = mpsc::sync_channel(0);
						sender = Some(sender_);
						let sockets: Ptr<*const _> = Ptr(&**sockets);
						catcher = Some(thread::spawn(move || {
							use constellation_internal::PidInternal;
							use std::io::Write;
							let mut now = Instant::now();
							let until = now + Duration::new(60, 0);
							while now < until {
								#[allow(clippy::match_same_arms)]
								match receiver.recv_timeout(until - now) {
									Ok(()) => return,
									Err(mpsc::RecvTimeoutError::Timeout) => (),
									Err(mpsc::RecvTimeoutError::Disconnected) => (), // panic!("omg")
								}
								now = Instant::now();
							}
							std::io::stderr()
								.write_all(
									format!(
										"\n{}: {}: {}: sockets: {:?}\n",
										super::pid(),
										nix::unistd::getpid(),
										super::pid().addr(),
										unsafe { &*sockets.0 }
									)
									.as_bytes(),
								)
								.unwrap(); // called after rust runtime exited, not sure what trace does
						}));
					}
					#[allow(clippy::cognitive_complexity)]
					notifier.wait(|_events, data| {
						if let Some(sender) = sender.take() {
							let _ = sender.send(());
							drop(sender);
							catcher.take().unwrap().join().unwrap();
						}
						if data == Key(ptr::null()) {
							for (remote, connection) in
								listener.poll(&notifier.context(Key(ptr::null())), &mut accept_hook)
							{
								let is_done = done.is_some();
								let mut sockets_ = if done.is_none() {
									Some(sockets.write().unwrap())
								} else {
									None
								};
								let sockets = done.as_mut().map_or_else(
									|| &mut **sockets_.as_mut().unwrap(),
									|x| &mut **x,
								);
								match sockets.entry(remote) {
									hash_map::Entry::Occupied(channel_) => {
										let channel_ = &**channel_.get(); // &**sockets.get(&remote).unwrap();
								  // if let &Inner::Connected(ref e) =
								  // 	&channel_.read().unwrap().as_ref().unwrap().inner
								  // {
								  // 	trace!("{:?} {:?} {:?}", e, local, remote);
								  // 	continue;
								  // }
										let notifier_key: *const RwLock<Option<Channel>> = channel_;
										let notifier =
											&notifier.context(Key(notifier_key as *const ()));
										let connectee: Connection = connection(notifier).into();
										let mut channel = channel_.write().unwrap();
										let channel = channel.as_mut().unwrap();
										if channel.inner.add_incoming(notifier).is_some() {
											channel.inner.add_incoming(notifier).unwrap()(
												connectee,
											);
										} else if channel.inner.closed() {
											let mut inner = Inner::connect(
												*local,
												remote,
												Some(connectee),
												notifier,
											);
											if is_done {
												if inner.closable() {
													inner.close(notifier);
												}
												if inner.drainable() {
													inner.drain(notifier);
												}
											}
											if !inner.closed() {
												channel.inner = inner;
											}
										} else {
											panic!("{:?} {:?} {:?}", channel, local, remote);
										}
										channel.inner.poll(notifier);
										if channel.inner.closable()
											&& !channel.inner.connecting() && !channel
											.inner
											.recvable()
										{
											channel.inner.close(notifier); // if the other end's process is ending; this could be given sooner
										}
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
											for receiver_future in
												channel.receivers_futures.drain(..)
											{
												receiver_future.wake();
											}
										} else {
											if channel.inner.closable() {
												channel.inner.close(notifier);
											}
											if channel.inner.drainable() {
												channel.inner.drain(notifier);
											}
										}
									}
									hash_map::Entry::Vacant(vacant) => {
										let channel = Arc::new(RwLock::new(None));
										let notifier_key: *const RwLock<Option<Channel>> =
											&*channel;
										let notifier =
											&notifier.context(Key(notifier_key as *const ()));
										let connectee: Connection = connection(notifier).into();
										let mut inner = Inner::connect(
											*local,
											remote,
											Some(connectee),
											notifier,
										);
										if is_done {
											if inner.closable() {
												inner.close(notifier);
											}
											if inner.drainable() {
												inner.drain(notifier);
											}
										}
										if !inner.closed() {
											*channel.try_write().unwrap() =
												Some(Channel::new(inner));
											let _ = vacant.insert(channel);
										}
									}
								}
							}
						} else if data != Key(1 as *const ()) {
							let is_done = done.is_some();
							let mut sockets = done.as_mut().map_or_else(
								|| Either::Left(sockets.write().unwrap()),
								|x| Either::Right(&mut **x),
							);
							let notifier_key: *const RwLock<Option<Channel>> = data.0 as *const _;
							let notifier = &notifier.context(Key(notifier_key as *const ()));
							// assert!(sockets.values().any(|channel|{
							// 	let notifier_key2: *const RwLock<Option<Channel>> = &**channel;
							// 	notifier_key2 == notifier_key
							// }));
							// let mut channel = unsafe{&*notifier_key}.write().unwrap();
							let channel_arc = sockets.values().find(|&channel| {
								let notifier_key2: *const RwLock<Option<Channel>> = &**channel;
								notifier_key2 == notifier_key
							});
							if let Some(channel_arc) = channel_arc {
								let mut channel = channel_arc.write().unwrap();
								assert_eq!(
									Arc::strong_count(&channel_arc),
									1 + channel.as_ref().unwrap().senders_count
										+ channel.as_ref().unwrap().receivers_count
								);
								let finished = {
									let channel: &mut Channel = channel.as_mut().unwrap();
									let inner: &mut Inner = &mut channel.inner;
									inner.poll(notifier);
									if inner.closable() && !inner.connecting() && !inner.recvable()
									{
										inner.close(notifier); // if the other end's process is ending; this could be given sooner
									}
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
										if inner.closable() {
											inner.close(notifier);
										}
										if inner.drainable() {
											inner.drain(notifier);
										}
									}
									channel.senders_count == 0
										&& channel.receivers_count == 0 && inner.closed()
								};
								if finished {
									let x = channel.take().unwrap();
									assert!(
										x.senders_count == 0
											&& x.receivers_count == 0 && x.inner.closed()
									);
									let key = *sockets
										.iter()
										.find(|&(_key, channel)| {
											let notifier_key2: *const RwLock<Option<Channel>> =
												&**channel;
											notifier_key2 == notifier_key
										})
										.unwrap()
										.0;
									drop(channel);
									let mut x =
										Arc::try_unwrap(sockets.remove(&key).unwrap()).unwrap();
									assert!(x.get_mut().unwrap().is_none());
								}
							}
						} else {
							assert!(done.is_none());
							// trace!("\\close"); // called after rust runtime exited, not sure what trace does
							// triggeree.triggered();
							drop(triggeree.take().unwrap());
							done = Some(sockets.write().unwrap());
							let sockets = &mut **done.as_mut().unwrap();
							for inner in sockets.values_mut() {
								let notifier_key: *const RwLock<Option<Channel>> = &**inner;
								let notifier = &notifier.context(Key(notifier_key as *const ()));
								let mut channel = inner.write().unwrap();
								let channel: &mut Channel = channel.as_mut().unwrap();
								let inner: &mut Inner = &mut channel.inner;
								if inner.closable() {
									inner.close(notifier);
								}
								if inner.drainable() {
									inner.drain(notifier);
								}
							}
						}
					});
				}
				// trace!("/close"); // called after rust runtime exited, not sure what trace does
			})
			.unwrap();
		Handle {
			triggerer: Some(triggerer),
			tcp_thread: Some(tcp_thread),
		}
	}
}
impl Drop for Reactor {
	fn drop(&mut self) {
		// trace!("drop context"); // called after rust runtime exited, not sure what trace does
		self.listener
			.get_mut()
			.unwrap()
			.take()
			.unwrap()
			.close(&self.notifier.context(Key(ptr::null())));
	}
}

#[derive(Debug)]
pub struct Channel {
	inner: Inner,
	senders_count: usize,
	receivers_count: usize,
	senders: HashMap<ThreadId, Thread>, // TODO: linked list
	senders_futures: Vec<Waker>,
	receivers: HashMap<ThreadId, Thread>,
	receivers_futures: Vec<Waker>,
}
impl Channel {
	fn new(inner: Inner) -> Self {
		Self {
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

/// Channel operation error modes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChannelError {
	/// The remote process has exited, thus `send()`/`recv()` could never succeed.
	Exited,
	/// The remote process terminated abruptly, or the channel was killed by the OS or hardware.
	Error,
}
impl fmt::Display for ChannelError {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			Self::Error => write!(f, "Remote process died or channel killed by OS/hardware"), //(ref err) => err.fmt(f),
			Self::Exited => write!(f, "Remote process already exited"),
		}
	}
}
impl error::Error for ChannelError {
	fn description(&self) -> &str {
		match *self {
			Self::Error => "remote process died or channel killed by OS/hardware", //(ref err) => err.description(),
			Self::Exited => "remote process already exited",
		}
	}

	fn cause(&self) -> Option<&dyn error::Error> {
		match *self {
			Self::Error /*(ref err) => Some(err),*/ |
			Self::Exited => None,
		}
	}
}

pub struct Sender<T: Serialize> {
	channel: Option<Arc<RwLock<Option<Channel>>>>,
	_marker: marker::PhantomData<fn(T)>,
}
impl<T: Serialize> Sender<T> {
	pub fn new(remote: SocketAddr, context: &Reactor) -> Option<Self> {
		let (notifier, sockets, local) = (&context.notifier, &context.sockets, &context.local);
		let sockets = &mut *sockets.write().unwrap();
		let channel = match sockets.entry(remote) {
			hash_map::Entry::Vacant(vacant) => {
				let channel = Arc::new(RwLock::new(None));
				let notifier_key: *const RwLock<Option<Channel>> = &*channel;
				let notifier = &notifier.context(Key(notifier_key as *const ()));
				let mut inner = Channel::new(Inner::connect(*local, remote, None, notifier));
				inner.senders_count += 1;
				*channel.try_write().unwrap() = Some(inner);
				let _ = vacant.insert(channel.clone());
				trace!("new sender {:?}", notifier_key);
				channel
			}
			hash_map::Entry::Occupied(mut channel) => {
				let channel = channel.get_mut(); // sockets.get_mut(&remote).unwrap();
				if channel.write().unwrap().as_mut().unwrap().senders_count > 0 {
					return None;
				}
				channel.write().unwrap().as_mut().unwrap().senders_count += 1;
				let notifier_key: *const RwLock<Option<Channel>> = &**channel;
				trace!("retain sender {:?}", notifier_key);
				channel.clone()
			}
		};
		assert_eq!(
			Arc::strong_count(&channel),
			1 + {
				let channel = channel.read().unwrap();
				channel.as_ref().unwrap().senders_count + channel.as_ref().unwrap().receivers_count
			}
		);
		Some(Self {
			channel: Some(channel),
			_marker: marker::PhantomData,
		})
	}

	pub fn async_send<'a, C: Borrow<Reactor> + 'a>(
		&'a self, context: C,
	) -> Option<impl FnOnce(T) + 'a>
	where
		T: 'static,
	{
		let mut channel = self.channel.as_ref().unwrap().write().unwrap();
		let unblocked = {
			// let notifier = &context.borrow().notifier;
			// let notifier_key: *const RwLock<Option<Channel>> =
			// 	&**self.channel.as_ref().unwrap();
			// let notifier = &notifier.context(Key(notifier_key as *const ()));
			// assert_eq!(Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
			let inner = &mut channel.as_mut().unwrap().inner;
			inner.send_avail().unwrap_or(!inner.valid()) // || inner.closed()
		};
		if unblocked {
			Some(move |t| {
				let _ = channel
					.as_mut()
					.unwrap()
					.senders
					.remove(&thread::current().id()); //.unwrap();
				let notifier = &context.borrow().notifier;
				let notifier_key: *const RwLock<Option<Channel>> =
					&**self.channel.as_ref().unwrap();
				let notifier = &notifier.context(Key(notifier_key as *const ()));
				let inner = &mut channel.as_mut().unwrap().inner;
				if !inner.valid() {
					panic!(".send() called on killed Sender");
				}
				if !inner.sendable() {
					panic!(".send() called on a closed Sender");
				}
				inner.send(t, notifier);
				// TODO: unpark queue?
			})
		} else {
			None
		}
	}

	pub fn selectable_send<'a, F: FnOnce() -> T + 'a>(&'a self, f: F) -> Send<'a, T, F>
	where
		T: 'static,
	{
		Send(self, RwLock::new(Some(f)))
	}

	pub fn drop(mut self, context: &Reactor) {
		let mut sockets = context.sockets.write().unwrap();
		let channel_arc = self.channel.take().unwrap();
		mem::forget(self);
		let notifier_key: *const RwLock<Option<Channel>> = &*channel_arc;
		let mut channel = channel_arc.write().unwrap();
		assert_eq!(
			Arc::strong_count(&channel_arc),
			1 + channel.as_ref().unwrap().senders_count + channel.as_ref().unwrap().receivers_count,
		);
		let finished = {
			let channel = channel.as_mut().unwrap();
			channel.senders_count -= 1;
			assert_eq!(channel.senders_count, 0);
			trace!("release sender {:?}", notifier_key);
			channel.senders_count == 0 && channel.receivers_count == 0 && channel.inner.closed()
		};
		if finished {
			let x = channel.take().unwrap();
			assert!(x.senders_count == 0 && x.receivers_count == 0 && x.inner.closed());
			let key = *sockets
				.iter()
				.find(|&(_key, channel)| {
					let notifier_key2: *const RwLock<Option<Channel>> = &**channel;
					notifier_key2 == notifier_key
				})
				.unwrap()
				.0;
			drop(channel);
			assert_eq!(Arc::strong_count(&channel_arc), 2);
			drop(channel_arc);
			trace!("drop sender {:?}", notifier_key);
			let mut x = Arc::try_unwrap(sockets.remove(&key).unwrap()).unwrap();
			assert!(x.get_mut().unwrap().is_none());
			trace!("channel.try_unwrap drop 1 success");
		}
	}
}
impl<T: Serialize> Sender<Option<T>> {
	pub fn futures_poll_ready(&self, cx: &mut Context, context: &Reactor) -> Poll<Result<(), Never>>
	where
		T: 'static,
	{
		self.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.senders_futures
			.push(cx.waker().clone());
		if let Some(_send) = self.async_send(context) {
			// TODO: remove from senders_futures
			Poll::Ready(Ok(()))
		} else {
			Poll::Pending
		}
	}

	pub fn futures_start_send(&self, item: T, context: &Reactor) -> Result<(), Never>
	where
		T: 'static,
	{
		self.async_send(context).expect(
			"called futures::Sink::start_send without the go-ahead from futures::Sink::poll_ready",
		)(Some(item));
		Ok(())
	}

	pub fn futures_poll_close(&self, cx: &mut Context, context: &Reactor) -> Poll<Result<(), Never>>
	where
		T: 'static,
	{
		self.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.senders_futures
			.push(cx.waker().clone());
		if let Some(send) = self.async_send(context) {
			// TODO: remove from senders_futures
			send(None);
			Poll::Ready(Ok(()))
		} else {
			Poll::Pending
		}
	}
}
impl<T: Serialize> Drop for Sender<T> {
	fn drop(&mut self) {
		panic!("call .drop(context) rather than dropping a Sender<T>");
	}
}
pub struct Send<'a, T: Serialize + 'static, F: FnOnce() -> T>(
	pub &'a Sender<T>,
	pub RwLock<Option<F>>,
);
impl<'a, T: Serialize + 'static, F: FnOnce() -> T> fmt::Debug for Send<'a, T, F> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Send").field("sender", &self.0).finish()
	}
}
impl<'a, T: Serialize + 'static, F: FnOnce() -> T> Selectable for Send<'a, T, F> {
	fn subscribe(&self, thread: Thread) {
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

	fn available<'b>(&'b mut self, context: &'b Reactor) -> Option<Box<dyn FnOnce() + 'b>> {
		self.0
			.async_send(context)
			.map(|t| -> Box<dyn FnOnce() + 'b> {
				Box::new(move || {
					let f = self.1.get_mut().unwrap().take().unwrap();
					t(f())
				})
			})
	}

	fn unsubscribe(&self, thread: Thread) {
		let _ = self
			.0
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
impl<'a, T: Serialize + 'static, F: FnOnce() -> T> Send<'a, T, F> {
	pub fn futures_poll(self: Pin<&mut Self>, cx: &mut Context, context: &Reactor) -> Poll<()> {
		self.0
			.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.senders_futures
			.push(cx.waker().clone());
		if let Some(send) = self.0.async_send(context) {
			// TODO: remove from senders_futures
			send(self.as_ref().1.write().unwrap().take().unwrap()());
			Poll::Ready(())
		} else {
			Poll::Pending
		}
	}
}

impl<T: Serialize> fmt::Debug for Sender<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Sender")
			.field("inner", &self.channel)
			.finish()
	}
}

pub struct Receiver<T: DeserializeOwned> {
	channel: Option<Arc<RwLock<Option<Channel>>>>,
	_marker: marker::PhantomData<fn() -> T>,
}
impl<T: DeserializeOwned> Receiver<T> {
	pub fn new(remote: SocketAddr, context: &Reactor) -> Option<Self> {
		let (notifier, sockets, local) = (&context.notifier, &context.sockets, &context.local);
		let sockets = &mut *sockets.write().unwrap();
		let channel = match sockets.entry(remote) {
			hash_map::Entry::Vacant(vacant) => {
				let channel = Arc::new(RwLock::new(None));
				let notifier_key: *const RwLock<Option<Channel>> = &*channel;
				let notifier = &notifier.context(Key(notifier_key as *const ()));
				let mut inner = Channel::new(Inner::connect(*local, remote, None, notifier));
				inner.receivers_count += 1;
				*channel.try_write().unwrap() = Some(inner);
				let _ = vacant.insert(channel.clone());
				trace!("new receiver {:?}", notifier_key);
				channel
			}
			hash_map::Entry::Occupied(mut channel) => {
				let channel = channel.get_mut(); // sockets.get_mut(&remote).unwrap();
				if channel.write().unwrap().as_mut().unwrap().receivers_count > 0 {
					return None;
				}
				channel.write().unwrap().as_mut().unwrap().receivers_count += 1;
				let notifier_key: *const RwLock<Option<Channel>> = &**channel;
				trace!("retain receiver {:?}", notifier_key);
				channel.clone()
			}
		};
		assert_eq!(
			Arc::strong_count(&channel),
			1 + {
				let channel = channel.read().unwrap();
				channel.as_ref().unwrap().senders_count + channel.as_ref().unwrap().receivers_count
			}
		);
		Some(Self {
			channel: Some(channel),
			_marker: marker::PhantomData,
		})
	}

	pub fn async_recv<'a, C: Borrow<Reactor> + 'a>(
		&'a self, context: C,
	) -> Option<impl FnOnce() -> Result<T, ChannelError> + 'a>
	where
		T: 'static,
	{
		let mut channel = self.channel.as_ref().unwrap().write().unwrap();
		let unblocked = {
			let notifier = &context.borrow().notifier;
			let notifier_key: *const RwLock<Option<Channel>> = &**self.channel.as_ref().unwrap();
			let notifier = &notifier.context(Key(notifier_key as *const ()));
			// assert_eq!(Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
			let inner = &mut channel.as_mut().unwrap().inner;
			inner.recv_avail::<T, _>(notifier).unwrap_or(!inner.valid()) // || inner.closed()
		};
		if unblocked {
			Some(move || {
				let _ = channel
					.as_mut()
					.unwrap()
					.receivers
					.remove(&thread::current().id()); //.unwrap();
				let notifier = &context.borrow().notifier;
				let notifier_key: *const RwLock<Option<Channel>> =
					&**self.channel.as_ref().unwrap();
				let notifier = &notifier.context(Key(notifier_key as *const ()));
				// let mut channel = self.channel.as_ref().unwrap().write().unwrap();
				// assert_eq!(Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
				let inner = &mut channel.as_mut().unwrap().inner;
				if !inner.valid() {
					return Err(ChannelError::Error);
				}
				if !inner.recvable() {
					return Err(ChannelError::Exited);
				}
				Ok(inner.recv(notifier))
				// TODO: unpark queue?
			})
		} else {
			None
		}
	}

	pub fn selectable_recv<'a, F: FnOnce(Result<T, ChannelError>) + 'a>(
		&'a self, f: F,
	) -> Recv<'a, T, F>
	where
		T: 'static,
	{
		Recv(self, RwLock::new(Some(f)))
	}

	pub fn drop(mut self, context: &Reactor) {
		let mut sockets = context.sockets.write().unwrap();
		let channel_arc = self.channel.take().unwrap();
		mem::forget(self);
		let notifier_key: *const RwLock<Option<Channel>> = &*channel_arc;
		let mut channel = channel_arc.write().unwrap();
		assert_eq!(
			Arc::strong_count(&channel_arc),
			1 + channel.as_ref().unwrap().senders_count + channel.as_ref().unwrap().receivers_count
		);
		let finished = {
			let channel = channel.as_mut().unwrap();
			channel.receivers_count -= 1;
			assert_eq!(channel.receivers_count, 0);
			trace!("release receiver {:?}", notifier_key);
			channel.senders_count == 0 && channel.receivers_count == 0 && channel.inner.closed()
		};
		if finished {
			let x = channel.take().unwrap();
			assert!(x.senders_count == 0 && x.receivers_count == 0 && x.inner.closed());
			let key = *sockets
				.iter()
				.find(|&(_key, channel)| {
					let notifier_key2: *const RwLock<Option<Channel>> = &**channel;
					notifier_key2 == notifier_key
				})
				.unwrap()
				.0;
			drop(channel);
			assert_eq!(Arc::strong_count(&channel_arc), 2);
			drop(channel_arc);
			trace!("drop receiver {:?}", notifier_key);
			let mut x = Arc::try_unwrap(sockets.remove(&key).unwrap()).unwrap();
			assert!(x.get_mut().unwrap().is_none());
			trace!("channel.try_unwrap drop 2 success");
		}
	}
}
impl<T: DeserializeOwned> Receiver<Option<T>> {
	pub fn futures_poll_next(
		&self, cx: &mut Context, context: &Reactor,
	) -> Poll<Option<Result<T, ChannelError>>>
	where
		T: 'static,
	{
		self.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.receivers_futures
			.push(cx.waker().clone());
		if let Some(recv) = self.async_recv(context) {
			// TODO: remove from receivers_futures
			Poll::Ready(match recv() {
				Ok(Some(t)) => Some(Ok(t)),
				Ok(None) => None,
				Err(err) => Some(Err(err)),
			})
		} else {
			Poll::Pending
		}
	}
}
impl<T: DeserializeOwned> Drop for Receiver<T> {
	fn drop(&mut self) {
		panic!("call .drop(context) rather than dropping a Receiver<T>");
	}
}
pub struct Recv<'a, T: DeserializeOwned + 'static, F: FnOnce(Result<T, ChannelError>)>(
	pub &'a Receiver<T>,
	pub RwLock<Option<F>>,
);
impl<'a, T: DeserializeOwned + 'static, F: FnOnce(Result<T, ChannelError>)> fmt::Debug
	for Recv<'a, T, F>
{
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Recv").field("receiver", &self.0).finish()
	}
}
impl<'a, T: DeserializeOwned + 'static, F: FnOnce(Result<T, ChannelError>)> Selectable
	for Recv<'a, T, F>
{
	fn subscribe(&self, thread: Thread) {
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

	fn available<'b>(&'b mut self, context: &'b Reactor) -> Option<Box<dyn FnOnce() + 'b>> {
		self.0
			.async_recv(context)
			.map(|t| -> Box<dyn FnOnce() + 'b> {
				Box::new(move || {
					let f = self.1.write().unwrap().take().unwrap();
					f(t())
				})
			})
	}

	fn unsubscribe(&self, thread: Thread) {
		let _ = self
			.0
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
impl<'a, T: DeserializeOwned + 'static, F: FnOnce(Result<T, ChannelError>)> Recv<'a, T, F> {
	pub fn futures_poll(self: Pin<&mut Self>, cx: &mut Context, context: &Reactor) -> Poll<()>
	where
		T: 'static,
	{
		self.0
			.channel
			.as_ref()
			.unwrap()
			.write()
			.unwrap()
			.as_mut()
			.unwrap()
			.receivers_futures
			.push(cx.waker().clone());
		if let Some(recv) = self.0.async_recv(context) {
			// TODO: remove from receivers_futures
			self.as_ref().1.write().unwrap().take().unwrap()(recv());
			Poll::Ready(())
		} else {
			Poll::Pending
		}
	}
}

impl<T: DeserializeOwned> fmt::Debug for Receiver<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Receiver")
			.field("inner", &self.channel)
			.finish()
	}
}

/// Types that can be [`select()`](select)ed upon.
///
/// [`select()`](select) lets you block on multiple blocking operations until progress can be made on at least one.
///
/// [`Receiver::selectable_recv()`](Receiver::selectable_recv) and [`Sender::selectable_send()`](Sender::selectable_send) let one create `Selectable` objects, any number of which can be passed to [`select()`](select). [`select()`](select) then blocks until at least one is progressable, and then from any that are progressable picks one at random and executes it.
///
/// It is inspired by the [`select()`](select) of go, which itself draws from David May's language [occam](https://en.wikipedia.org/wiki/Occam_(programming_language)) and Tony Hoare’s formalisation of [Communicating Sequential Processes](https://en.wikipedia.org/wiki/Communicating_sequential_processes).
pub trait Selectable: fmt::Debug {
	#[doc(hidden)]
	fn subscribe(&self, thread: Thread);
	#[doc(hidden)]
	// type State;
	#[doc(hidden)]
	fn available<'a>(&'a mut self, context: &'a Reactor) -> Option<Box<dyn FnOnce() + 'a>>;
	// #[doc(hidden)]
	// fn run(&mut self, state: Self::State); // get rid once impl trait works in trait method return vals
	#[doc(hidden)]
	fn unsubscribe(&self, thread: Thread);
}
// struct SelectableRun<'a,T:Selectable+?Sized+'a>(&'a mut T,<T as Selectable>::State);
// impl<'a,T:Selectable+?Sized+'a> ops::FnOnce<()> for SelectableRun<'a,T> {
// 	type Output = String;
// 	extern "rust-call" fn call_once(self, args: ()) -> Self::Output {
// 		self.0.run(self.1)
// 	}
// }
// pub fn select<'a, F: FnMut() -> C, C: Borrow<Reactor>>(
// 	mut select: Vec<Box<dyn Selectable + 'a>>, context: &mut F,
// ) -> impl Iterator<Item = Box<dyn Selectable + 'a>> + 'a {
// 	for selectable in &select {
// 		selectable.subscribe(thread::current());
// 	}
// 	let mut context_lock;
// 	let ret = loop {
// 		let mut rand = Rand::new();
// 		context_lock = Some(context());
// 		for (i, selectable) in select.iter_mut().enumerate() {
// 			if let Some(run) = selectable.available(context_lock.as_ref().unwrap().borrow()) {
// 				rand.push((i, run), &mut rand::thread_rng());
// 			}
// 		}
// 		if let Some((i, run)) = rand.get() {
// 			break (i, run);
// 		}
// 		drop(context_lock.take().unwrap());
// 		thread::park();
// 	};
// 	let i_ = ret.0;
// 	{ ret }.1();
// 	for (i, selectable) in select.iter().enumerate() {
// 		// TODO: unsub should be before run
// 		if i != i_ {
// 			selectable.unsubscribe(thread::current());
// 		}
// 	}
// 	drop(context_lock.take().unwrap());
// 	let mut rem = Vec::with_capacity(select.len() - 1);
// 	for (i, select) in select.into_iter().enumerate() {
// 		if i != i_ {
// 			rem.push(select);
// 			// } else {
// 			// ret.1();
// 			// select.run(&*context());
// 		}
// 	}
// 	rem.into_iter()
// }
