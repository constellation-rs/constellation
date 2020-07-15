#![allow(clippy::large_enum_variant)]

use super::{
	Connection, InnerClosing, InnerClosingPoll, InnerConnected, InnerConnectedPoll, InnerConnecting, InnerConnectingLocalClosed, InnerConnectingLocalClosedPoll, InnerConnectingPoll, InnerLocalClosed, InnerLocalClosedPoll, InnerRemoteClosed, InnerRemoteClosedPoll
};
use serde::{de::DeserializeOwned, Serialize};
use std::{mem, net::SocketAddr};
use tcp_typed::Notifier;

#[derive(Debug)]
pub enum Inner {
	Connecting(InnerConnecting),
	ConnectingLocalClosed(InnerConnectingLocalClosed),
	Connected(InnerConnected),
	RemoteClosed(InnerRemoteClosed),
	LocalClosed(InnerLocalClosed),
	Closing(InnerClosing),
	Closed,
	Killed,
}
impl Inner {
	pub fn connect(
		bind: SocketAddr, local: SocketAddr, remote: SocketAddr, incoming: Option<Connection>,
		notifier: &impl Notifier,
	) -> Self {
		InnerConnecting::new(bind, local, remote, incoming, notifier).into()
	}

	pub fn poll(&mut self, notifier: &impl Notifier) {
		*self = match mem::replace(self, Self::Killed) {
			Self::Connecting(connecting) => connecting.poll(notifier).into(),
			Self::ConnectingLocalClosed(connecting_local_closed) => {
				connecting_local_closed.poll(notifier).into()
			}
			Self::Connected(connected) => connected.poll(notifier).into(),
			Self::RemoteClosed(remote_closed) => remote_closed.poll(notifier).into(),
			Self::LocalClosed(local_closed) => local_closed.poll(notifier).into(),
			Self::Closing(closing) => closing.poll(notifier).into(),
			Self::Closed => Self::Closed,
			Self::Killed => Self::Killed,
		};
	}

	pub fn add_incoming<'a>(
		&'a mut self, notifier: &'a impl Notifier,
	) -> Option<impl FnOnce(Connection) + 'a> {
		match self {
			Self::Connecting(_) | Self::ConnectingLocalClosed(_) => {
				Some(move |connection| match self {
					Self::Connecting(connecting) => connecting.add_incoming(connection, notifier),
					Self::ConnectingLocalClosed(connecting_local_closed) => {
						connecting_local_closed.add_incoming(connection, notifier)
					}
					_ => unreachable!(),
				})
			}
			_ => None,
		}
	}

	pub fn connecting(&self) -> bool {
		match *self {
			Self::Connecting(_) | Self::ConnectingLocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn recvable(&self) -> bool {
		match *self {
			Self::Connected(_) | Self::LocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn recv_avail<T: DeserializeOwned + 'static, E: Notifier>(
		&mut self, notifier: &E,
	) -> Option<bool> {
		if self.recvable() {
			Some(match *self {
				Self::Connected(ref mut connected) => connected.recv_avail::<T, E>(notifier),
				Self::LocalClosed(ref mut local_closed) => {
					local_closed.recv_avail::<T, E>(notifier)
				}
				_ => unreachable!(),
			})
		} else {
			None
		}
	}

	pub fn recv<T: DeserializeOwned + 'static>(&mut self, notifier: &impl Notifier) -> T {
		match *self {
			Self::Connected(ref mut connected) => connected.recv(notifier),
			Self::LocalClosed(ref mut local_closed) => local_closed.recv(notifier),
			_ => panic!(),
		}
	}

	pub fn drainable(&self) -> bool {
		match *self {
			Self::Connected(_) | Self::LocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn drain(&mut self, notifier: &impl Notifier) {
		*self = match mem::replace(self, Self::Killed) {
			Self::Connected(connected) => connected.drain(notifier).into(),
			Self::LocalClosed(local_closed) => local_closed.drain(notifier).into(),
			_ => panic!(),
		};
	}

	pub fn sendable(&self) -> bool {
		match *self {
			Self::Connected(_) | Self::RemoteClosed(_) => true,
			_ => false,
		}
	}

	pub fn send_avail(&self) -> Option<bool> {
		if self.sendable() {
			Some(match *self {
				Self::Connected(ref connected) => connected.send_avail(),
				Self::RemoteClosed(ref remote_closed) => remote_closed.send_avail(),
				_ => unreachable!(),
			})
		} else {
			None
		}
	}

	pub fn send<T: Serialize + 'static>(&mut self, x: T, notifier: &impl Notifier) {
		match *self {
			Self::Connected(ref mut connected) => connected.send(x, notifier),
			Self::RemoteClosed(ref mut remote_closed) => remote_closed.send(x, notifier),
			_ => panic!(),
		}
	}

	pub fn closable(&self) -> bool {
		match *self {
			Self::Connecting(_) | Self::Connected(_) | Self::RemoteClosed(_) => true,
			_ => false,
		}
	}

	pub fn closed(&self) -> bool {
		match *self {
			Self::Closed => true,
			_ => false,
		}
	}

	pub fn valid(&self) -> bool {
		match *self {
			Self::Connecting(_)
			| Self::ConnectingLocalClosed(_)
			| Self::Connected(_)
			| Self::RemoteClosed(_)
			| Self::LocalClosed(_)
			| Self::Closing(_)
			| Self::Closed => true,
			Self::Killed => false,
		}
	}

	pub fn close(&mut self, notifier: &impl Notifier) {
		*self = match mem::replace(self, Self::Killed) {
			Self::Connecting(connecting) => connecting.close(notifier).into(),
			Self::Connected(connected) => connected.close(notifier).into(),
			Self::RemoteClosed(remote_closed) => remote_closed.close(notifier).into(),
			Self::ConnectingLocalClosed(_)
			| Self::LocalClosed(_)
			| Self::Closing(_)
			| Self::Closed
			| Self::Killed => panic!(),
		};
	}
	// pub fn drop(self, notifier: &impl Notifier) {
}

impl From<InnerConnecting> for Inner {
	#[inline(always)]
	fn from(connecter: InnerConnecting) -> Self {
		Self::Connecting(connecter)
	}
}
impl From<InnerConnectingPoll> for Inner {
	#[inline(always)]
	fn from(connecting_poll: InnerConnectingPoll) -> Self {
		match connecting_poll {
			InnerConnectingPoll::Connecting(connecting) => Self::Connecting(connecting),
			InnerConnectingPoll::Connected(connected) => Self::Connected(connected),
			InnerConnectingPoll::RemoteClosed(remote_closed) => Self::RemoteClosed(remote_closed),
			InnerConnectingPoll::Killed => Self::Killed,
		}
	}
}
impl From<InnerConnectingLocalClosed> for Inner {
	#[inline(always)]
	fn from(connecting_local_closed: InnerConnectingLocalClosed) -> Self {
		Self::ConnectingLocalClosed(connecting_local_closed)
	}
}
impl From<InnerConnectingLocalClosedPoll> for Inner {
	#[inline(always)]
	fn from(connecter_local_closed_poll: InnerConnectingLocalClosedPoll) -> Self {
		match connecter_local_closed_poll {
			InnerConnectingLocalClosedPoll::ConnectingLocalClosed(connecting_local_closed) => {
				Self::ConnectingLocalClosed(connecting_local_closed)
			}
			InnerConnectingLocalClosedPoll::LocalClosed(local_closed) => {
				Self::LocalClosed(local_closed)
			}
			InnerConnectingLocalClosedPoll::Closing(closing) => Self::Closing(closing),
			InnerConnectingLocalClosedPoll::Closed => Self::Closed,
			InnerConnectingLocalClosedPoll::Killed => Self::Killed,
		}
	}
}
impl From<InnerConnected> for Inner {
	#[inline(always)]
	fn from(connected: InnerConnected) -> Self {
		Self::Connected(connected)
	}
}
impl From<InnerConnectedPoll> for Inner {
	#[inline(always)]
	fn from(connected_poll: InnerConnectedPoll) -> Self {
		match connected_poll {
			InnerConnectedPoll::Connected(connected) => Self::Connected(connected),
			InnerConnectedPoll::RemoteClosed(remote_closed) => Self::RemoteClosed(remote_closed),
			InnerConnectedPoll::Killed => Self::Killed,
		}
	}
}
impl From<InnerRemoteClosed> for Inner {
	#[inline(always)]
	fn from(remote_closed: InnerRemoteClosed) -> Self {
		Self::RemoteClosed(remote_closed)
	}
}
impl From<InnerRemoteClosedPoll> for Inner {
	#[inline(always)]
	fn from(remote_closed_poll: InnerRemoteClosedPoll) -> Self {
		match remote_closed_poll {
			InnerRemoteClosedPoll::RemoteClosed(remote_closed) => Self::RemoteClosed(remote_closed),
			InnerRemoteClosedPoll::Killed => Self::Killed,
		}
	}
}
impl From<InnerLocalClosed> for Inner {
	#[inline(always)]
	fn from(local_closed: InnerLocalClosed) -> Self {
		Self::LocalClosed(local_closed)
	}
}
impl From<InnerLocalClosedPoll> for Inner {
	#[inline(always)]
	fn from(local_closed_poll: InnerLocalClosedPoll) -> Self {
		match local_closed_poll {
			InnerLocalClosedPoll::LocalClosed(local_closed) => Self::LocalClosed(local_closed),
			InnerLocalClosedPoll::Closing(closing) => Self::Closing(closing),
			InnerLocalClosedPoll::Closed => Self::Closed,
			InnerLocalClosedPoll::Killed => Self::Killed,
		}
	}
}
impl From<InnerClosing> for Inner {
	#[inline(always)]
	fn from(closing: InnerClosing) -> Self {
		Self::Closing(closing)
	}
}
impl From<InnerClosingPoll> for Inner {
	#[inline(always)]
	fn from(closing_poll: InnerClosingPoll) -> Self {
		match closing_poll {
			InnerClosingPoll::Closing(closing) => Self::Closing(closing),
			InnerClosingPoll::Closed => Self::Closed,
			InnerClosingPoll::Killed => Self::Killed,
		}
	}
}
