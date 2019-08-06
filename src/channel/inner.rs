#![allow(clippy::large_enum_variant)]

use super::*;
use serde::{de::DeserializeOwned, Serialize};
use std::net::SocketAddr;
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
		local: SocketAddr, remote: SocketAddr, incoming: Option<Connection>,
		notifier: &impl Notifier,
	) -> Self {
		InnerConnecting::new(local, remote, incoming, notifier).into()
	}

	pub fn poll(&mut self, notifier: &impl Notifier) {
		*self = match mem::replace(self, Inner::Killed) {
			Inner::Connecting(connecting) => connecting.poll(notifier).into(),
			Inner::ConnectingLocalClosed(connecting_local_closed) => {
				connecting_local_closed.poll(notifier).into()
			}
			Inner::Connected(connected) => connected.poll(notifier).into(),
			Inner::RemoteClosed(remote_closed) => remote_closed.poll(notifier).into(),
			Inner::LocalClosed(local_closed) => local_closed.poll(notifier).into(),
			Inner::Closing(closing) => closing.poll(notifier).into(),
			Inner::Closed => Inner::Closed,
			Inner::Killed => Inner::Killed,
		};
	}

	pub fn add_incoming<'a>(
		&'a mut self, notifier: &'a impl Notifier,
	) -> Option<impl FnOnce(Connection) + 'a> {
		match self {
			Inner::Connecting(_) | Inner::ConnectingLocalClosed(_) => {
				Some(move |connection| match self {
					Inner::Connecting(connecting) => connecting.add_incoming(connection, notifier),
					Inner::ConnectingLocalClosed(connecting_local_closed) => {
						connecting_local_closed.add_incoming(connection, notifier)
					}
					_ => unreachable!(),
				})
			}
			_ => None,
		}
	}

	pub fn connecting(&self) -> bool {
		match self {
			&Inner::Connecting(_) | &Inner::ConnectingLocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn recvable(&self) -> bool {
		match self {
			&Inner::Connected(_) | &Inner::LocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn recv_avail<T: DeserializeOwned + 'static, E: Notifier>(
		&mut self, notifier: &E,
	) -> Option<bool> {
		if self.recvable() {
			Some(match self {
				&mut Inner::Connected(ref mut connected) => connected.recv_avail::<T, E>(notifier),
				&mut Inner::LocalClosed(ref mut local_closed) => {
					local_closed.recv_avail::<T, E>(notifier)
				}
				_ => unreachable!(),
			})
		} else {
			None
		}
	}

	pub fn recv<T: DeserializeOwned + 'static>(&mut self, notifier: &impl Notifier) -> T {
		match self {
			&mut Inner::Connected(ref mut connected) => connected.recv(notifier),
			&mut Inner::LocalClosed(ref mut local_closed) => local_closed.recv(notifier),
			_ => panic!(),
		}
	}

	pub fn drainable(&self) -> bool {
		match self {
			&Inner::Connected(_) | &Inner::LocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn drain(&mut self, notifier: &impl Notifier) {
		*self = match mem::replace(self, Inner::Killed) {
			Inner::Connected(connected) => connected.drain(notifier).into(),
			Inner::LocalClosed(local_closed) => local_closed.drain(notifier).into(),
			_ => panic!(),
		};
	}

	pub fn sendable(&self) -> bool {
		match self {
			&Inner::Connected(_) | &Inner::RemoteClosed(_) => true,
			_ => false,
		}
	}

	pub fn send_avail(&self) -> Option<bool> {
		if self.sendable() {
			Some(match self {
				&Inner::Connected(ref connected) => connected.send_avail(),
				&Inner::RemoteClosed(ref remote_closed) => remote_closed.send_avail(),
				_ => unreachable!(),
			})
		} else {
			None
		}
	}

	pub fn send<T: Serialize + 'static>(&mut self, x: T, notifier: &impl Notifier) {
		match self {
			&mut Inner::Connected(ref mut connected) => connected.send(x, notifier),
			&mut Inner::RemoteClosed(ref mut remote_closed) => remote_closed.send(x, notifier),
			_ => panic!(),
		}
	}

	pub fn closable(&self) -> bool {
		match self {
			&Inner::Connecting(_) | &Inner::Connected(_) | &Inner::RemoteClosed(_) => true,
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
			&Inner::Connecting(_)
			| &Inner::ConnectingLocalClosed(_)
			| &Inner::Connected(_)
			| &Inner::RemoteClosed(_)
			| &Inner::LocalClosed(_)
			| &Inner::Closing(_)
			| &Inner::Closed => true,
			&Inner::Killed => false,
		}
	}

	pub fn close(&mut self, notifier: &impl Notifier) {
		*self = match mem::replace(self, Inner::Killed) {
			Inner::Connecting(connecting) => connecting.close(notifier).into(),
			Inner::Connected(connected) => connected.close(notifier).into(),
			Inner::RemoteClosed(remote_closed) => remote_closed.close(notifier).into(),
			Inner::ConnectingLocalClosed(_)
			| Inner::LocalClosed(_)
			| Inner::Closing(_)
			| Inner::Closed
			| Inner::Killed => panic!(),
		};
	}
	// pub fn drop(self, notifier: &impl Notifier) {
}

impl From<InnerConnecting> for Inner {
	#[inline(always)]
	fn from(connecter: InnerConnecting) -> Self {
		Inner::Connecting(connecter)
	}
}
impl From<InnerConnectingPoll> for Inner {
	#[inline(always)]
	fn from(connecting_poll: InnerConnectingPoll) -> Self {
		match connecting_poll {
			InnerConnectingPoll::Connecting(connecting) => Inner::Connecting(connecting),
			InnerConnectingPoll::Connected(connected) => Inner::Connected(connected),
			InnerConnectingPoll::RemoteClosed(remote_closed) => Inner::RemoteClosed(remote_closed),
			InnerConnectingPoll::Killed => Inner::Killed,
		}
	}
}
impl From<InnerConnectingLocalClosed> for Inner {
	#[inline(always)]
	fn from(connecting_local_closed: InnerConnectingLocalClosed) -> Self {
		Inner::ConnectingLocalClosed(connecting_local_closed)
	}
}
impl From<InnerConnectingLocalClosedPoll> for Inner {
	#[inline(always)]
	fn from(connecter_local_closed_poll: InnerConnectingLocalClosedPoll) -> Self {
		match connecter_local_closed_poll {
			InnerConnectingLocalClosedPoll::ConnectingLocalClosed(connecting_local_closed) => {
				Inner::ConnectingLocalClosed(connecting_local_closed)
			}
			InnerConnectingLocalClosedPoll::LocalClosed(local_closed) => {
				Inner::LocalClosed(local_closed)
			}
			InnerConnectingLocalClosedPoll::Closing(closing) => Inner::Closing(closing),
			InnerConnectingLocalClosedPoll::Closed => Inner::Closed,
			InnerConnectingLocalClosedPoll::Killed => Inner::Killed,
		}
	}
}
impl From<InnerConnected> for Inner {
	#[inline(always)]
	fn from(connected: InnerConnected) -> Self {
		Inner::Connected(connected)
	}
}
impl From<InnerConnectedPoll> for Inner {
	#[inline(always)]
	fn from(connected_poll: InnerConnectedPoll) -> Self {
		match connected_poll {
			InnerConnectedPoll::Connected(connected) => Inner::Connected(connected),
			InnerConnectedPoll::RemoteClosed(remote_closed) => Inner::RemoteClosed(remote_closed),
			InnerConnectedPoll::Killed => Inner::Killed,
		}
	}
}
impl From<InnerRemoteClosed> for Inner {
	#[inline(always)]
	fn from(remote_closed: InnerRemoteClosed) -> Self {
		Inner::RemoteClosed(remote_closed)
	}
}
impl From<InnerRemoteClosedPoll> for Inner {
	#[inline(always)]
	fn from(remote_closed_poll: InnerRemoteClosedPoll) -> Self {
		match remote_closed_poll {
			InnerRemoteClosedPoll::RemoteClosed(remote_closed) => {
				Inner::RemoteClosed(remote_closed)
			}
			InnerRemoteClosedPoll::Killed => Inner::Killed,
		}
	}
}
impl From<InnerLocalClosed> for Inner {
	#[inline(always)]
	fn from(local_closed: InnerLocalClosed) -> Self {
		Inner::LocalClosed(local_closed)
	}
}
impl From<InnerLocalClosedPoll> for Inner {
	#[inline(always)]
	fn from(local_closed_poll: InnerLocalClosedPoll) -> Self {
		match local_closed_poll {
			InnerLocalClosedPoll::LocalClosed(local_closed) => Inner::LocalClosed(local_closed),
			InnerLocalClosedPoll::Closing(closing) => Inner::Closing(closing),
			InnerLocalClosedPoll::Closed => Inner::Closed,
			InnerLocalClosedPoll::Killed => Inner::Killed,
		}
	}
}
impl From<InnerClosing> for Inner {
	#[inline(always)]
	fn from(closing: InnerClosing) -> Self {
		Inner::Closing(closing)
	}
}
impl From<InnerClosingPoll> for Inner {
	#[inline(always)]
	fn from(closing_poll: InnerClosingPoll) -> Self {
		match closing_poll {
			InnerClosingPoll::Closing(closing) => Inner::Closing(closing),
			InnerClosingPoll::Closed => Inner::Closed,
			InnerClosingPoll::Killed => Inner::Killed,
		}
	}
}
