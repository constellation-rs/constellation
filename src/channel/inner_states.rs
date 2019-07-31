#![allow(clippy::large_enum_variant)]

use super::*;
use serde::{de::DeserializeOwned, Serialize};
use std::{
	collections::hash_map::DefaultHasher, hash::{Hash, Hasher}, net::SocketAddr
};
use tcp_typed::Notifier;

/// Used to determine which side should be connecter/client and which connectee/server/listener.
fn ord(a: &SocketAddr, b: &SocketAddr) -> bool {
	let a = (a.ip(), a.port());
	let b = (b.ip(), b.port());
	assert_ne!(a, b);
	let mut a_hasher = DefaultHasher::new();
	let mut b_hasher = DefaultHasher::new();
	loop {
		a.hash(&mut a_hasher);
		let a_hash = a_hasher.finish();
		b.hash(&mut b_hasher);
		let b_hash = b_hasher.finish();
		if b_hash != a_hash {
			break b_hash > a_hash;
		}
	}
}

#[derive(Debug)]
pub enum InnerConnectingPoll {
	Connecting(InnerConnecting),
	Connected(InnerConnected),
	RemoteClosed(InnerRemoteClosed),
	Killed,
}
#[derive(Debug)]
pub enum InnerConnecting {
	Outgoing(Option<Connection>),
	Incoming(Option<Connection>),
}
impl InnerConnecting {
	pub fn new(
		local: SocketAddr, remote: SocketAddr, incoming: Option<Connection>,
		notifier: &impl Notifier,
	) -> InnerConnectingPoll {
		if ord(&local, &remote) {
			assert!(incoming.is_none());
			InnerConnecting::Outgoing(Some(Connection::connect(local, remote, notifier)))
		} else {
			InnerConnecting::Incoming(incoming)
		}
		.poll(notifier)
	}

	pub fn add_incoming(&mut self, incoming: Connection, notifier: &impl Notifier) {
		if let InnerConnecting::Incoming(ref mut prev_incoming) = self {
			if let Some(mut prev_incoming) = prev_incoming.take() {
				prev_incoming.kill(notifier).unwrap()();
			}
			*prev_incoming = Some(incoming);
			notifier.queue();
		} else {
			panic!();
		}
	}

	pub fn poll(mut self, notifier: &impl Notifier) -> InnerConnectingPoll {
		match self {
			InnerConnecting::Incoming(ref mut incoming) => {
				if incoming.is_some() {
					incoming.as_mut().unwrap().poll(notifier);
					if !incoming.as_ref().unwrap().connecting() {
						return match InnerConnected::new(incoming.take().unwrap(), notifier) {
							InnerConnectedPoll::Connected(connected) => {
								InnerConnectingPoll::Connected(connected)
							}
							InnerConnectedPoll::RemoteClosed(remote_closed) => {
								InnerConnectingPoll::RemoteClosed(remote_closed)
							}
							InnerConnectedPoll::Killed => InnerConnectingPoll::Killed,
						};
					}
				}
			}
			InnerConnecting::Outgoing(ref mut outgoing) => {
				if outgoing.is_some() {
					outgoing.as_mut().unwrap().poll(notifier);
					if !outgoing.as_ref().unwrap().connecting() {
						return match InnerConnected::new(outgoing.take().unwrap(), notifier) {
							InnerConnectedPoll::Connected(connected) => {
								InnerConnectingPoll::Connected(connected)
							}
							InnerConnectedPoll::RemoteClosed(remote_closed) => {
								InnerConnectingPoll::RemoteClosed(remote_closed)
							}
							InnerConnectedPoll::Killed => InnerConnectingPoll::Killed,
						};
					}
				}
			}
		}
		InnerConnectingPoll::Connecting(self)
	}

	pub fn close(self, notifier: &impl Notifier) -> InnerConnectingLocalClosedPoll {
		InnerConnectingLocalClosed::new(
			match self {
				InnerConnecting::Outgoing(outgoing) => Either::Left(outgoing),
				InnerConnecting::Incoming(incoming) => Either::Left(incoming),
			},
			notifier,
		)
	}
}

#[derive(Debug)]
pub enum InnerConnectingLocalClosedPoll {
	ConnectingLocalClosed(InnerConnectingLocalClosed),
	LocalClosed(InnerLocalClosed),
	Closing(InnerClosing),
	Closed,
	Killed,
}
#[derive(Debug)]
pub enum InnerConnectingLocalClosed {
	Outgoing(Option<Connection>),
	Incoming(Option<Connection>),
}
impl InnerConnectingLocalClosed {
	fn new(
		connection: Either<Option<Connection>, Option<Connection>>, notifier: &impl Notifier,
	) -> InnerConnectingLocalClosedPoll {
		match connection {
			Either::Left(outgoing) => InnerConnectingLocalClosed::Outgoing(outgoing),
			Either::Right(incoming) => InnerConnectingLocalClosed::Incoming(incoming),
		}
		.poll(notifier)
	}

	pub fn add_incoming(&mut self, incoming: Connection, notifier: &impl Notifier) {
		if let InnerConnectingLocalClosed::Incoming(ref mut prev_incoming) = self {
			if let Some(mut prev_incoming) = prev_incoming.take() {
				prev_incoming.kill(notifier).unwrap()();
			}
			*prev_incoming = Some(incoming);
			notifier.queue();
		} else {
			panic!();
		}
	}

	pub fn poll(mut self, notifier: &impl Notifier) -> InnerConnectingLocalClosedPoll {
		match self {
			InnerConnectingLocalClosed::Incoming(ref mut incoming) => {
				if incoming.is_some() {
					incoming.as_mut().unwrap().poll(notifier);
					if incoming.as_ref().unwrap().closable() {
						incoming.as_mut().unwrap().close(notifier).unwrap()();
					}
					if !incoming.as_ref().unwrap().connecting() {
						return match InnerLocalClosed::new(
							incoming.take().unwrap(),
							serde_pipe::Serializer::new(),
							serde_pipe::Deserializer::new(),
							false,
							notifier,
						) {
							InnerLocalClosedPoll::LocalClosed(local_closed) => {
								InnerConnectingLocalClosedPoll::LocalClosed(local_closed)
							}
							InnerLocalClosedPoll::Closing(closing) => {
								InnerConnectingLocalClosedPoll::Closing(closing)
							}
							InnerLocalClosedPoll::Closed => InnerConnectingLocalClosedPoll::Closed,
							InnerLocalClosedPoll::Killed => InnerConnectingLocalClosedPoll::Killed,
						};
					}
				}
				if incoming.is_none() {
					return InnerConnectingLocalClosedPoll::Closed;
				}
			}
			InnerConnectingLocalClosed::Outgoing(ref mut outgoing) => {
				if outgoing.is_some() {
					outgoing.as_mut().unwrap().poll(notifier);
					if outgoing.as_ref().unwrap().closable() {
						outgoing.as_mut().unwrap().close(notifier).unwrap()();
					}
					if !outgoing.as_ref().unwrap().connecting() {
						return match InnerLocalClosed::new(
							outgoing.take().unwrap(),
							serde_pipe::Serializer::new(),
							serde_pipe::Deserializer::new(),
							false,
							notifier,
						) {
							InnerLocalClosedPoll::LocalClosed(local_closed) => {
								InnerConnectingLocalClosedPoll::LocalClosed(local_closed)
							}
							InnerLocalClosedPoll::Closing(closing) => {
								InnerConnectingLocalClosedPoll::Closing(closing)
							}
							InnerLocalClosedPoll::Closed => InnerConnectingLocalClosedPoll::Closed,
							InnerLocalClosedPoll::Killed => InnerConnectingLocalClosedPoll::Killed,
						};
					}
				}
				if outgoing.is_none() {
					return InnerConnectingLocalClosedPoll::Closed;
				}
			}
		}
		InnerConnectingLocalClosedPoll::ConnectingLocalClosed(self)
	}
}

#[derive(Debug)]
pub enum InnerConnectedPoll {
	Connected(InnerConnected),
	RemoteClosed(InnerRemoteClosed),
	Killed,
}
#[derive(Debug)]
pub struct InnerConnected {
	connection: Connection,
	send_serializer: serde_pipe::Serializer,
	recv_deserializer: serde_pipe::Deserializer,
	recv_deserializer_given: bool,
}
impl InnerConnected {
	fn new(connection: Connection, notifier: &impl Notifier) -> InnerConnectedPoll {
		Self {
			connection,
			send_serializer: serde_pipe::Serializer::new(),
			recv_deserializer: serde_pipe::Deserializer::new(),
			recv_deserializer_given: false,
		}
		.poll(notifier)
	}

	pub fn poll(mut self, notifier: &impl Notifier) -> InnerConnectedPoll {
		assert!(!self.connection.connecting());
		let mut progress = true;
		loop {
			if self.connection.sendable() {
				while self.connection.send_avail().unwrap() > 0 && self.send_serializer.pull_avail()
				{
					self.connection.send(notifier).unwrap()(self.send_serializer.pull().unwrap()());
					progress = true;
				}
			}
			if self.connection.recvable() {
				while self.connection.recv_avail().unwrap() > 0
					&& self.recv_deserializer.push_avail()
				{
					self.recv_deserializer.push().unwrap()(
						self.connection.recv(notifier).unwrap()(),
					);
					progress = true;
				}
			}
			if !progress {
				break;
			}
			progress = false;
			self.connection.poll(notifier);
		}
		if !self.connection.recvable() && self.recv_deserializer.empty().is_none() {
			return match InnerRemoteClosed::new(
				self.connection,
				self.send_serializer,
				false,
				notifier,
			) {
				InnerRemoteClosedPoll::RemoteClosed(remote_closed) => {
					InnerConnectedPoll::RemoteClosed(remote_closed)
				}
				InnerRemoteClosedPoll::Killed => InnerConnectedPoll::Killed,
			};
		}
		if !self.connection.valid() {
			return InnerConnectedPoll::Killed;
		}
		InnerConnectedPoll::Connected(self)
	}

	pub fn send_avail(&self) -> bool {
		self.send_serializer.push_avail()
	}

	pub fn send<T: Serialize + 'static>(&mut self, t: T, notifier: &impl Notifier) {
		self.send_serializer.push().unwrap()(t);
		notifier.queue();
	}

	pub fn recv_avail<T: DeserializeOwned + 'static, E: Notifier>(&mut self, notifier: &E) -> bool {
		if !self.recv_deserializer_given {
			self.recv_deserializer_given = true;
			notifier.queue(); // TODO: we only actually need to do this if self.poll() is gonna return Either::Right
		}
		self.recv_deserializer.pull::<T>().is_some()
	}

	pub fn recv<T: DeserializeOwned + 'static>(&mut self, notifier: &impl Notifier) -> T {
		self.recv_deserializer_given = false;
		let ret = self.recv_deserializer.pull::<T>().unwrap()();
		notifier.queue();
		ret
	}

	pub fn drain(mut self, notifier: &impl Notifier) -> InnerRemoteClosedPoll {
		if let Some(empty) = self.recv_deserializer.empty() {
			empty();
		}
		InnerRemoteClosed::new(self.connection, self.send_serializer, true, notifier)
	}

	pub fn close(self, notifier: &impl Notifier) -> InnerLocalClosedPoll {
		InnerLocalClosed::new(
			self.connection,
			self.send_serializer,
			self.recv_deserializer,
			self.recv_deserializer_given,
			notifier,
		)
	}
}

#[derive(Debug)]
pub enum InnerRemoteClosedPoll {
	RemoteClosed(InnerRemoteClosed),
	Killed,
}
#[derive(Debug)]
pub struct InnerRemoteClosed {
	connection: Connection,
	send_serializer: serde_pipe::Serializer,
	drain: bool,
}
impl InnerRemoteClosed {
	fn new(
		connection: Connection, send_serializer: serde_pipe::Serializer, drain: bool,
		notifier: &impl Notifier,
	) -> InnerRemoteClosedPoll {
		Self {
			connection,
			send_serializer,
			drain,
		}
		.poll(notifier)
	}

	pub fn poll(mut self, notifier: &impl Notifier) -> InnerRemoteClosedPoll {
		if self.drain && !self.connection.recvable() {
			self.drain = false;
		}
		if self.drain {
			let mut progress = false;
			while self.connection.recv_avail().unwrap() > 0 {
				let _ = self.connection.recv(notifier).unwrap()();
				progress = true;
			}
			if progress {
				self.connection.poll(notifier);
			}
			if !self.connection.recvable() {
				self.drain = false;
			}
		} else {
			assert!(!self.connection.recvable());
		}
		let mut progress = true;
		loop {
			if self.connection.sendable() {
				while self.connection.send_avail().unwrap() > 0 && self.send_serializer.pull_avail()
				{
					self.connection.send(notifier).unwrap()(self.send_serializer.pull().unwrap()());
					progress = true;
				}
			}
			if !progress {
				break;
			}
			progress = false;
			self.connection.poll(notifier);
		}
		if !self.connection.valid() {
			return InnerRemoteClosedPoll::Killed;
		}
		InnerRemoteClosedPoll::RemoteClosed(self)
	}

	pub fn send_avail(&self) -> bool {
		self.send_serializer.push_avail()
	}

	pub fn send<T: Serialize + 'static>(&mut self, t: T, notifier: &impl Notifier) {
		self.send_serializer.push().unwrap()(t);
		notifier.queue();
	}

	pub fn close(self, notifier: &impl Notifier) -> InnerClosingPoll {
		InnerClosing::new(self.connection, self.send_serializer, false, notifier)
	}
}

#[derive(Debug)]
pub enum InnerLocalClosedPoll {
	LocalClosed(InnerLocalClosed),
	Closing(InnerClosing),
	Closed,
	Killed,
}
#[derive(Debug)]
pub struct InnerLocalClosed {
	connection: Connection,
	send_serializer: serde_pipe::Serializer,
	recv_deserializer: serde_pipe::Deserializer,
	recv_deserializer_given: bool,
}
impl InnerLocalClosed {
	fn new(
		connection: Connection, send_serializer: serde_pipe::Serializer,
		recv_deserializer: serde_pipe::Deserializer, recv_deserializer_given: bool,
		notifier: &impl Notifier,
	) -> InnerLocalClosedPoll {
		Self {
			connection,
			send_serializer,
			recv_deserializer,
			recv_deserializer_given,
		}
		.poll(notifier)
	}

	pub fn poll(mut self, notifier: &impl Notifier) -> InnerLocalClosedPoll {
		assert!(!self.connection.connecting());
		let mut progress = true;
		loop {
			if self.connection.sendable() {
				while self.connection.send_avail().unwrap() > 0 && self.send_serializer.pull_avail()
				{
					self.connection.send(notifier).unwrap()(self.send_serializer.pull().unwrap()());
					progress = true;
				}
			}
			if self.connection.recvable() {
				while self.connection.recv_avail().unwrap() > 0
					&& self.recv_deserializer.push_avail()
				{
					self.recv_deserializer.push().unwrap()(
						self.connection.recv(notifier).unwrap()(),
					);
					progress = true;
				}
			}
			if !progress {
				break;
			}
			progress = false;
			self.connection.poll(notifier);
		}
		if self.connection.sendable() && !self.send_serializer.pull_avail() {
			self.connection.close(notifier).unwrap()();
		}
		// if !self.connection.recvable() {
		// 	assert!(!self.recv_deserializer.push_avail(), "{}: CLOSED WHILE PENDING RECV", ::internal::pid());
		// }
		if !self.connection.recvable() && self.recv_deserializer.empty().is_none() {
			// self.recv_deserializer.pull_avail() {
			// assert!(!self.recv_deserializer_given);
			return match InnerClosing::new(self.connection, self.send_serializer, false, notifier) {
				InnerClosingPoll::Closing(closing) => InnerLocalClosedPoll::Closing(closing),
				InnerClosingPoll::Closed => InnerLocalClosedPoll::Closed,
				InnerClosingPoll::Killed => InnerLocalClosedPoll::Killed,
			};
		}
		if !self.connection.valid() {
			return InnerLocalClosedPoll::Killed;
		}
		InnerLocalClosedPoll::LocalClosed(self)
	}

	pub fn recv_avail<T: DeserializeOwned + 'static, E: Notifier>(&mut self, notifier: &E) -> bool {
		if !self.recv_deserializer_given {
			self.recv_deserializer_given = true;
			notifier.queue(); // TODO: we only actually need to do this if self.poll() is gonna return Either::Right
		}
		self.recv_deserializer.pull::<T>().is_some()
	}

	pub fn recv<T: DeserializeOwned + 'static>(&mut self, notifier: &impl Notifier) -> T {
		self.recv_deserializer_given = false;
		let ret = self.recv_deserializer.pull::<T>().unwrap()();
		notifier.queue();
		ret
	}

	pub fn drain(mut self, notifier: &impl Notifier) -> InnerClosingPoll {
		if let Some(empty) = self.recv_deserializer.empty() {
			empty();
		}
		InnerClosing::new(self.connection, self.send_serializer, true, notifier)
	}
}

#[derive(Debug)]
pub enum InnerClosingPoll {
	Closing(InnerClosing),
	Closed,
	Killed,
}
#[derive(Debug)]
pub struct InnerClosing {
	connection: Connection,
	send_serializer: serde_pipe::Serializer,
	drain: bool,
}
impl InnerClosing {
	fn new(
		connection: Connection, send_serializer: serde_pipe::Serializer, drain: bool,
		notifier: &impl Notifier,
	) -> InnerClosingPoll {
		Self {
			connection,
			send_serializer,
			drain,
		}
		.poll(notifier)
	}

	pub fn poll(mut self, notifier: &impl Notifier) -> InnerClosingPoll {
		if self.drain && !self.connection.recvable() {
			self.drain = false;
		}
		if self.drain {
			let mut progress = false;
			while self.connection.recv_avail().unwrap() > 0 {
				let _ = self.connection.recv(notifier).unwrap()();
				progress = true;
			}
			if progress {
				self.connection.poll(notifier);
			}
			if !self.connection.recvable() {
				self.drain = false;
			}
		} else {
			assert!(!self.connection.recvable());
		}
		let mut progress = true;
		loop {
			if self.connection.sendable() {
				while self.connection.send_avail().unwrap() > 0 && self.send_serializer.pull_avail()
				{
					self.connection.send(notifier).unwrap()(self.send_serializer.pull().unwrap()());
					progress = true;
				}
			}
			if !progress {
				break;
			}
			progress = false;
			self.connection.poll(notifier);
		}
		if self.connection.sendable() && !self.send_serializer.pull_avail() {
			self.connection.close(notifier).unwrap()();
		}
		if !self.connection.valid() {
			return InnerClosingPoll::Killed;
		}
		if self.connection.closed() {
			return InnerClosingPoll::Closed;
		}
		InnerClosingPoll::Closing(self)
	}
}
