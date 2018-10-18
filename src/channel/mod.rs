mod inner;
mod inner_states;

use constellation_internal::Rand;
use either::Either;
// use futures;
use nix::sys::socket;
use notifier::{Notifier, Triggerer};
use palaver::spawn;
use rand;
use serde;
use serde_pipe;
use std::{
	borrow::Borrow, boxed::FnBox, cell, collections::{hash_map, HashMap}, error, fmt, marker, mem, net, os, ptr, sync::{self, Arc}, thread
};
use tcp_typed::{Connection, Listener};

#[cfg(target_family = "unix")]
type Fd = os::unix::io::RawFd;
#[cfg(target_family = "windows")]
type Fd = os::windows::io::RawHandle;

pub use self::{inner::*, inner_states::*};
pub use tcp_typed::{socket_forwarder, SocketForwardee, SocketForwarder};

#[derive(Copy, Clone, PartialEq, Eq)]
struct Key(*const ());
/// Because `*const ()`. Pointers aren't really not Send+Sync, it's more of a lint.
unsafe impl marker::Send for Key {}
unsafe impl Sync for Key {}
impl From<usize> for Key {
	fn from(x: usize) -> Self {
		Key(x as *const ())
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
	listener: sync::RwLock<Option<Listener>>,
	sockets: sync::RwLock<HashMap<net::SocketAddr, Arc<sync::RwLock<Option<Channel>>>>>,
	local: net::SocketAddr,
}
impl Reactor {
	pub fn new(host: net::IpAddr) -> (Self, u16) {
		let notifier = Notifier::new();
		let (listener, port) = Listener::new_ephemeral(&host, &notifier.context(Key(ptr::null())));
		let sockets = sync::RwLock::new(HashMap::new());
		let local = net::SocketAddr::new(host, port);
		(
			Self {
				notifier,
				listener: sync::RwLock::new(Some(listener)),
				sockets,
				local,
			},
			port,
		)
	}

	pub fn with_fd(fd: Fd) -> Self {
		let notifier = Notifier::new();
		let listener = Listener::with_fd(fd, &notifier.context(Key(ptr::null())));
		let sockets = sync::RwLock::new(HashMap::new());
		let local = if let socket::SockAddr::Inet(inet) = socket::getsockname(fd).unwrap() {
			inet.to_std()
		} else {
			panic!()
		};
		Self {
			notifier,
			listener: sync::RwLock::new(Some(listener)),
			sockets,
			local,
		}
	}

	pub fn with_forwardee(socket_forwardee: SocketForwardee, local: net::SocketAddr) -> Self {
		let notifier = Notifier::new();
		let listener =
			Listener::with_socket_forwardee(socket_forwardee, &notifier.context(Key(ptr::null())));
		let sockets = sync::RwLock::new(HashMap::new());
		Self {
			notifier,
			listener: sync::RwLock::new(Some(listener)),
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
		let tcp_thread = spawn(String::from("tcp-thread"), move || {
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
				sync::RwLockWriteGuard<
					HashMap<net::SocketAddr, Arc<sync::RwLock<Option<Channel>>>>,
				>,
			> = None;
			while done.is_none() || done.as_ref().unwrap().iter().any(|(_, ref inner)| {
				// TODO: maintain count
				let inner = inner.read().unwrap();
				let inner = &inner.as_ref().unwrap().inner;
				inner.valid() && !inner.closed()
			}) {
				// if let &Some(ref sockets) = &done {
				// 	trace!(
				// 		"sockets: {:?}",
				// 		&**sockets
				// 	); // called after rust runtime exited, not sure what trace does
				// }
				#[allow(clippy::cyclomatic_complexity)]
				notifier.wait(|_events, data| {
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
							let sockets = done
								.as_mut()
								.map_or_else(|| &mut **sockets_.as_mut().unwrap(), |x| &mut **x);
							match sockets.entry(remote) {
								hash_map::Entry::Occupied(channel_) => {
									let channel_ = &**channel_.get(); // &**sockets.get(&remote).unwrap();
										   // if let &Inner::Connected(ref e) =
										   // 	&channel_.read().unwrap().as_ref().unwrap().inner
										   // {
										   // 	trace!("{:?} {:?} {:?}", e, local, remote);
										   // 	continue;
										   // }
									let notifier_key: *const sync::RwLock<Option<Channel>> = channel_;
									let notifier =
										&notifier.context(Key(notifier_key as *const ()));
									let connectee: Connection = connection(notifier).into();
									let mut channel = channel_.write().unwrap();
									let channel = channel.as_mut().unwrap();
									if channel.inner.add_incoming(notifier).is_some() {
										channel.inner.add_incoming(notifier).unwrap()(connectee);
									} else if channel.inner.closed() {
										let mut inner = Inner::connect(
											*local,
											remote,
											Some(connectee),
											notifier,
										);
										if is_done && inner.closable() {
											inner.close(notifier);
										}
										if !inner.closed() {
											channel.inner = inner;
										}
									} else {
										panic!("{:?} {:?} {:?}", channel, local, remote);
									}
									channel.inner.poll(notifier);
									if !is_done {
										for sender in channel.senders.values() {
											sender.unpark(); // TODO: don't do unless actual progress
										}
										// for sender_future in channel.senders_futures.drain(..) {
										// 	sender_future.wake();
										// }
										for receiver in channel.receivers.values() {
											receiver.unpark(); // TODO: don't do unless actual progress
										}
									// for receiver_future in channel.receivers_futures.drain(..) {
									// 	receiver_future.wake();
									// }
									} else if channel.inner.closable() {
										channel.inner.close(notifier);
									}
								}
								hash_map::Entry::Vacant(vacant) => {
									let channel = Arc::new(sync::RwLock::new(None));
									let notifier_key: *const sync::RwLock<Option<Channel>> = &*channel;
									let notifier =
										&notifier.context(Key(notifier_key as *const ()));
									let connectee: Connection = connection(notifier).into();
									let mut inner =
										Inner::connect(*local, remote, Some(connectee), notifier);
									if is_done && inner.closable() {
										inner.close(notifier);
									}
									if !inner.closed() {
										*channel.try_write().unwrap() = Some(Channel::new(inner));
										let _ = vacant.insert(channel);
									}
								}
							}
						}
					} else if data != Key(1 as *const ()) {
						if done.is_none() {
							let mut sockets = sockets.write().unwrap();
							let notifier_key: *const sync::RwLock<
								Option<Channel>,
							> = data.0 as *const _;
							let notifier = &notifier.context(Key(notifier_key as *const ()));
							// assert!(sockets.values().any(|channel|{
							// 	let notifier_key2: *const sync::RwLock<Option<Channel>> = &**channel;
							// 	notifier_key2 == notifier_key
							// }));
							// let mut channel = unsafe{&*notifier_key}.write().unwrap();
							let channel_arc = sockets.values().find(|&channel| {
								let notifier_key2: *const sync::RwLock<Option<Channel>> = &**channel;
								notifier_key2 == notifier_key
							});
							if let Some(channel_arc) = channel_arc {
								let mut channel = channel_arc.write().unwrap();
								assert_eq!(
									sync::Arc::strong_count(&channel_arc),
									1 + channel.as_ref().unwrap().senders_count
										+ channel.as_ref().unwrap().receivers_count
								);
								let finished = {
									let channel: &mut Channel = channel.as_mut().unwrap();
									let inner: &mut Inner = &mut channel.inner;
									inner.poll(notifier);
									for sender in channel.senders.values() {
										sender.unpark(); // TODO: don't do unless actual progress
									}
									// for sender_future in channel.senders_futures.drain(..) {
									// 	sender_future.wake();
									// }
									for receiver in channel.receivers.values() {
										receiver.unpark(); // TODO: don't do unless actual progress
									}
									// for receiver_future in channel.receivers_futures.drain(..) {
									// 	receiver_future.wake();
									// }
									channel.senders_count == 0
										&& channel.receivers_count == 0
										&& inner.closed()
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
											let notifier_key2: *const sync::RwLock<Option<Channel>> = &**channel;
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
							let sockets = &mut **done.as_mut().unwrap();
							let notifier_key: *const sync::RwLock<
								Option<Channel>,
							> = data.0 as *const _;
							let notifier = &notifier.context(Key(notifier_key as *const ()));
							// assert!(sockets.values().any(|channel|{
							// 	let notifier_key2: *const sync::RwLock<Option<Channel>> = &**channel;
							// 	notifier_key2 == notifier_key
							// }));
							// let mut channel = unsafe{&*notifier_key}.write().unwrap();
							let channel_arc = sockets.values().find(|&channel| {
								let notifier_key2: *const sync::RwLock<Option<Channel>> = &**channel;
								notifier_key2 == notifier_key
							});
							if let Some(channel_arc) = channel_arc {
								let mut channel = channel_arc.write().unwrap();
								assert_eq!(
									sync::Arc::strong_count(&channel_arc),
									1 + channel.as_ref().unwrap().senders_count
										+ channel.as_ref().unwrap().receivers_count
								);
								let finished = {
									let channel: &mut Channel = channel.as_mut().unwrap();
									let inner: &mut Inner = &mut channel.inner;
									inner.poll(notifier);
									if inner.closable() {
										inner.close(notifier);
									}
									channel.senders_count == 0
										&& channel.receivers_count == 0
										&& inner.closed()
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
											let notifier_key2: *const sync::RwLock<Option<Channel>> = &**channel;
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
						}
					} else {
						assert!(done.is_none());
						// trace!("\\close"); // called after rust runtime exited, not sure what trace does
						// triggeree.triggered();
						drop(triggeree.take().unwrap());
						done = Some(sockets.write().unwrap());
						let sockets = &mut **done.as_mut().unwrap();
						for inner in sockets.values_mut() {
							let notifier_key: *const sync::RwLock<
								Option<Channel>,
							> = &**inner;
							let notifier = &notifier.context(Key(notifier_key as *const ()));
							let mut channel = inner.write().unwrap();
							let channel: &mut Channel = channel.as_mut().unwrap();
							let inner: &mut Inner = &mut channel.inner;
							if inner.closable() {
								inner.close(notifier);
							}
						}
					}
				});
			}
			// trace!("/close"); // called after rust runtime exited, not sure what trace does
		});
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
	senders: HashMap<thread::ThreadId, thread::Thread>, // TODO: linked list
	// senders_futures: Vec<futures::task::Waker>,
	receivers: HashMap<thread::ThreadId, thread::Thread>,
	// receivers_futures: Vec<futures::task::Waker>,
}
impl Channel {
	fn new(inner: Inner) -> Self {
		Self {
			inner,
			senders_count: 0,
			receivers_count: 0,
			senders: HashMap::new(),
			// senders_futures: Vec::new(),
			receivers: HashMap::new(),
			// receivers_futures: Vec::new(),
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
			ChannelError::Exited => "remote process already exited",
		}
	}

	fn cause(&self) -> Option<&error::Error> {
		match *self {
			ChannelError::Error /*(ref err) => Some(err),*/ |
			ChannelError::Exited => None,
		}
	}
}

pub struct Sender<T: serde::ser::Serialize> {
	channel: Option<Arc<sync::RwLock<Option<Channel>>>>,
	_marker: marker::PhantomData<fn(T)>,
}
impl<T: serde::ser::Serialize> Sender<T> {
	pub fn new(remote: net::SocketAddr, context: &Reactor) -> Option<Self> {
		let (notifier, sockets, local) = (&context.notifier, &context.sockets, &context.local);
		let sockets = &mut *sockets.write().unwrap();
		let channel = match sockets.entry(remote) {
			hash_map::Entry::Vacant(vacant) => {
				let channel = Arc::new(sync::RwLock::new(None));
				let notifier_key: *const sync::RwLock<Option<Channel>> = &*channel;
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
				let notifier_key: *const sync::RwLock<Option<Channel>> = &**channel;
				trace!("retain sender {:?}", notifier_key);
				channel.clone()
			}
		};
		assert_eq!(
			sync::Arc::strong_count(&channel),
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
		&'a self, context_: C,
	) -> Option<impl FnOnce(T) + 'a>
	where
		T: 'static,
	{
		let mut channel = self.channel.as_ref().unwrap().write().unwrap();
		let unblocked = {
			// let context = context_.borrow();
			// let notifier = &context.notifier;
			// let notifier_key: *const sync::RwLock<Option<Channel>> =
			// 	&**self.channel.as_ref().unwrap();
			// let notifier = &notifier.context(Key(notifier_key as *const ()));
			// assert_eq!(sync::Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
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
				let context = context_.borrow();
				let notifier = &context.notifier;
				let notifier_key: *const sync::RwLock<Option<Channel>> =
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

	pub fn send<F: FnMut() -> C, C: Borrow<Reactor>>(&self, t: T, context: &mut F)
	where
		T: 'static,
	{
		let x = cell::RefCell::new(None);
		let _ = select(
			vec![Box::new(self.selectable_send(|| {
				*x.borrow_mut() = Some(());
				t
			}))],
			context,
		);
		x.into_inner().unwrap()
	}

	pub fn selectable_send<'a, F: FnOnce() -> T + 'a>(&'a self, f: F) -> impl Selectable + 'a
	where
		T: 'static,
	{
		Send(self, Some(f))
	}

	pub fn drop(mut self, context: &Reactor) {
		let mut sockets = context.sockets.write().unwrap();
		let channel_arc = self.channel.take().unwrap();
		mem::forget(self);
		let notifier_key: *const sync::RwLock<Option<Channel>> = &*channel_arc;
		let mut channel = channel_arc.write().unwrap();
		assert_eq!(
			sync::Arc::strong_count(&channel_arc),
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
					let notifier_key2: *const sync::RwLock<Option<Channel>> = &**channel;
					notifier_key2 == notifier_key
				})
				.unwrap()
				.0;
			drop(channel);
			assert_eq!(sync::Arc::strong_count(&channel_arc), 2);
			drop(channel_arc);
			trace!("drop sender {:?}", notifier_key);
			let mut x = Arc::try_unwrap(sockets.remove(&key).unwrap()).unwrap();
			assert!(x.get_mut().unwrap().is_none());
			trace!("channel.try_unwrap drop 1 success");
		}
	}
}
// impl<T: serde::ser::Serialize> Sender<Option<T>> {
// 	pub fn futures_poll_ready(
// 		&self, cx: &futures::task::LocalWaker, context: &Reactor,
// 	) -> futures::task::Poll<Result<(), !>>
// 	where
// 		T: 'static,
// 	{
// 		self.channel
// 			.as_ref()
// 			.unwrap()
// 			.write()
// 			.unwrap()
// 			.as_mut()
// 			.unwrap()
// 			.senders_futures
// 			.push(cx.clone().into());
// 		if let Some(_send) = self.async_send(context) {
// 			// TODO: remove from senders_futures
// 			futures::task::Poll::Ready(Ok(()))
// 		} else {
// 			futures::task::Poll::Pending
// 		}
// 	}

// 	pub fn futures_start_send(&self, item: T, context: &Reactor) -> Result<(), !>
// 	where
// 		T: 'static,
// 	{
// 		self.async_send(context).expect(
// 			"called futures::Sink::start_send without the go-ahead from futures::Sink::poll_ready",
// 		)(Some(item));
// 		Ok(())
// 	}

// 	pub fn futures_poll_close(
// 		&self, cx: &futures::task::LocalWaker, context: &Reactor,
// 	) -> futures::task::Poll<Result<(), !>>
// 	where
// 		T: 'static,
// 	{
// 		self.channel
// 			.as_ref()
// 			.unwrap()
// 			.write()
// 			.unwrap()
// 			.as_mut()
// 			.unwrap()
// 			.senders_futures
// 			.push(cx.clone().into());
// 		if let Some(send) = self.async_send(context) {
// 			// TODO: remove from senders_futures
// 			send(None);
// 			futures::task::Poll::Ready(Ok(()))
// 		} else {
// 			futures::task::Poll::Pending
// 		}
// 	}
// }
impl<T: serde::ser::Serialize> Drop for Sender<T> {
	fn drop(&mut self) {
		panic!("call .drop(context) rather than dropping a Sender<T>");
	}
}
struct Send<'a, T: serde::ser::Serialize + 'static, F: FnOnce() -> T>(&'a Sender<T>, Option<F>);
impl<'a, T: serde::ser::Serialize + 'static, F: FnOnce() -> T> fmt::Debug for Send<'a, T, F> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Send").field("sender", &self.0).finish()
	}
}
impl<'a, T: serde::ser::Serialize + 'static, F: FnOnce() -> T> Selectable for Send<'a, T, F> {
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

	fn available<'b>(&'b mut self, context: &'b Reactor) -> Option<Box<FnBox() + 'b>> {
		self.0.async_send(context).map(|t| {
			Box::new(move || {
				let f = self.1.take().unwrap();
				t(f())
			}) as Box<FnBox() + 'b>
		})
	}

	fn unsubscribe(&self, thread: thread::Thread) {
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
	pub fn new(remote: net::SocketAddr, context: &Reactor) -> Option<Self> {
		let (notifier, sockets, local) = (&context.notifier, &context.sockets, &context.local);
		let sockets = &mut *sockets.write().unwrap();
		let channel = match sockets.entry(remote) {
			hash_map::Entry::Vacant(vacant) => {
				let channel = Arc::new(sync::RwLock::new(None));
				let notifier_key: *const sync::RwLock<Option<Channel>> = &*channel;
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
				let notifier_key: *const sync::RwLock<Option<Channel>> = &**channel;
				trace!("retain receiver {:?}", notifier_key);
				channel.clone()
			}
		};
		assert_eq!(
			sync::Arc::strong_count(&channel),
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
			let notifier_key: *const sync::RwLock<Option<Channel>> =
				&**self.channel.as_ref().unwrap();
			let notifier = &notifier.context(Key(notifier_key as *const ()));
			// assert_eq!(sync::Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
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
				let notifier_key: *const sync::RwLock<Option<Channel>> =
					&**self.channel.as_ref().unwrap();
				let notifier = &notifier.context(Key(notifier_key as *const ()));
				// let mut channel = self.channel.as_ref().unwrap().write().unwrap();
				// assert_eq!(sync::Arc::strong_count(&self.channel.as_ref().unwrap()), 1+channel.as_ref().unwrap().senders_count+channel.as_ref().unwrap().receivers_count);
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

	pub fn recv<F: FnMut() -> C, C: Borrow<Reactor>>(
		&self, context: &mut F,
	) -> Result<T, ChannelError>
	where
		T: 'static,
	{
		let x = cell::RefCell::new(None);
		let _ = select(
			vec![Box::new(
				self.selectable_recv(|t| *x.borrow_mut() = Some(t)),
			)],
			context,
		);
		x.into_inner().unwrap()
	}

	pub fn selectable_recv<'a, F: FnOnce(Result<T, ChannelError>) + 'a>(
		&'a self, f: F,
	) -> impl Selectable + 'a
	where
		T: 'static,
	{
		Recv(self, Some(f))
	}

	pub fn drop(mut self, context: &Reactor) {
		let mut sockets = context.sockets.write().unwrap();
		let channel_arc = self.channel.take().unwrap();
		mem::forget(self);
		let notifier_key: *const sync::RwLock<Option<Channel>> = &*channel_arc;
		let mut channel = channel_arc.write().unwrap();
		assert_eq!(
			sync::Arc::strong_count(&channel_arc),
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
					let notifier_key2: *const sync::RwLock<Option<Channel>> = &**channel;
					notifier_key2 == notifier_key
				})
				.unwrap()
				.0;
			drop(channel);
			assert_eq!(sync::Arc::strong_count(&channel_arc), 2);
			drop(channel_arc);
			trace!("drop receiver {:?}", notifier_key);
			let mut x = Arc::try_unwrap(sockets.remove(&key).unwrap()).unwrap();
			assert!(x.get_mut().unwrap().is_none());
			trace!("channel.try_unwrap drop 2 success");
		}
	}
}
// impl<T: serde::de::DeserializeOwned> Receiver<Option<T>> {
// 	pub fn futures_poll_next(
// 		&self, cx: &futures::task::LocalWaker, context: &Reactor,
// 	) -> futures::task::Poll<Option<Result<T, ChannelError>>>
// 	where
// 		T: 'static,
// 	{
// 		self.channel
// 			.as_ref()
// 			.unwrap()
// 			.write()
// 			.unwrap()
// 			.as_mut()
// 			.unwrap()
// 			.receivers_futures
// 			.push(cx.clone().into());
// 		if let Some(recv) = self.async_recv(context) {
// 			// TODO: remove from receivers_futures
// 			futures::task::Poll::Ready(match recv() {
// 				Ok(Some(t)) => Some(Ok(t)),
// 				Ok(None) => None,
// 				Err(err) => Some(Err(err)),
// 			})
// 		} else {
// 			futures::task::Poll::Pending
// 		}
// 	}
// }
impl<T: serde::de::DeserializeOwned> Drop for Receiver<T> {
	fn drop(&mut self) {
		panic!("call .drop(context) rather than dropping a Receiver<T>");
	}
}
struct Recv<'a, T: serde::de::DeserializeOwned + 'static, F: FnOnce(Result<T, ChannelError>)>(
	&'a Receiver<T>,
	Option<F>,
);
impl<'a, T: serde::de::DeserializeOwned + 'static, F: FnOnce(Result<T, ChannelError>)> fmt::Debug
	for Recv<'a, T, F>
{
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Recv").field("receiver", &self.0).finish()
	}
}
impl<'a, T: serde::de::DeserializeOwned + 'static, F: FnOnce(Result<T, ChannelError>)> Selectable
	for Recv<'a, T, F>
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

	fn available<'b>(&'b mut self, context: &'b Reactor) -> Option<Box<FnBox() + 'b>> {
		self.0.async_recv(context).map(|t| {
			Box::new(move || {
				let f = self.1.take().unwrap();
				f(t())
			}) as Box<FnBox() + 'b>
		})
	}

	fn unsubscribe(&self, thread: thread::Thread) {
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
impl<T: serde::de::DeserializeOwned> fmt::Debug for Receiver<T> {
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
	fn subscribe(&self, thread::Thread);
	#[doc(hidden)]
	// type State;
	#[doc(hidden)]
	fn available<'a>(&'a mut self, context: &'a Reactor) -> Option<Box<FnBox() + 'a>>;
	// #[doc(hidden)]
	// fn run(&mut self, state: Self::State); // get rid once impl trait works in trait method return vals
	#[doc(hidden)]
	fn unsubscribe(&self, thread::Thread);
}
// struct SelectableRun<'a,T:Selectable+?Sized+'a>(&'a mut T,<T as Selectable>::State);
// impl<'a,T:Selectable+?Sized+'a> ops::FnOnce<()> for SelectableRun<'a,T> {
// 	type Output = String;
// 	extern "rust-call" fn call_once(self, args: ()) -> Self::Output {
// 		self.0.run(self.1)
// 	}
// }
pub fn select<'a, F: FnMut() -> C, C: Borrow<Reactor>>(
	mut select: Vec<Box<Selectable + 'a>>, context: &mut F,
) -> impl Iterator<Item = Box<Selectable + 'a>> + 'a {
	for selectable in &select {
		selectable.subscribe(thread::current());
	}
	let mut context_lock;
	let ret = loop {
		let mut rand = Rand::new();
		context_lock = Some(context());
		for (i, selectable) in select.iter_mut().enumerate() {
			if let Some(run) = selectable.available(context_lock.as_ref().unwrap().borrow()) {
				rand.push((i, run), &mut rand::thread_rng());
			}
		}
		if let Some((i, run)) = rand.get() {
			break (i, run);
		}
		drop(context_lock.take().unwrap());
		thread::park();
	};
	let i_ = ret.0;
	{ ret }.1();
	for (i, selectable) in select.iter().enumerate() {
		// TODO: unsub should be before run
		if i != i_ {
			selectable.unsubscribe(thread::current());
		}
	}
	drop(context_lock.take().unwrap());
	let mut rem = Vec::with_capacity(select.len() - 1);
	for (i, select) in select.into_iter().enumerate() {
		if i != i_ {
			rem.push(select);
			// } else {
			// ret.1();
			// select.run(&*context());
		}
	}
	rem.into_iter()
}
