use super::heap;
use itertools;
use nix::{self, libc};

use std::{cmp, collections::HashSet, fmt, mem, net, ops, os, ptr, sync, time};

use either::Either;

use std::os::unix::io::IntoRawFd;

use deploy_common::PidInternal;

use super::circularbuffer::{
	CircularBuffer, CircularBufferReadToFd, CircularBufferWriteFromFd, Readable, ReadableFd, Writable, WritableFd
};

const INCOMING_MAGIC: u64 = 123456789098765432;
const OUTGOING_MAGIC: u64 = 223456789098765432;
const CONNECT_TIMEOUT: u64 = 60; // time::Duration::new(60,0);

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub struct EpollExecutorContext<'a> {
	executor: &'a EpollExecutor,
	key: *const (),
}
impl<'a> EpollExecutorContext<'a> {
	pub fn add_fd(&self, fd: os::unix::io::RawFd) {
		self.executor.add_fd(fd, self.key)
	}

	pub fn remove_fd(&self, fd: os::unix::io::RawFd) {
		self.executor.remove_fd(fd, self.key)
	}

	pub fn add_instant(&self, instant: time::Instant) -> heap::Slot {
		self.executor.add_instant(instant, self.key)
	}

	pub fn remove_instant(&self, slot: heap::Slot) {
		self.executor.remove_instant(slot)
	}

	pub fn add_trigger(&self) -> (Triggerer, Triggeree) {
		self.executor.add_trigger(self.key)
	}
}

struct TimeEvent(time::Instant, *const ());
unsafe impl Send for TimeEvent {}
unsafe impl Sync for TimeEvent {} // HACK for *const ()
impl PartialEq for TimeEvent {
	fn eq(&self, other: &Self) -> bool {
		self.0.eq(&other.0)
	}
}
impl Eq for TimeEvent {}
impl cmp::PartialOrd for TimeEvent {
	fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
		Some(self.0.cmp(&other.0))
	}
}
impl cmp::Ord for TimeEvent {
	fn cmp(&self, other: &Self) -> cmp::Ordering {
		self.0.cmp(&other.0)
	}
}
pub struct EpollExecutor {
	epoll: Epoll,
	timer: sync::RwLock<heap::Heap<TimeEvent>>,
}
impl EpollExecutor {
	pub fn new() -> EpollExecutor {
		EpollExecutor {
			epoll: Epoll::new(),
			timer: sync::RwLock::new(heap::Heap::new()),
		}
	}

	pub fn context<'a>(&'a self, key: *const ()) -> EpollExecutorContext<'a> {
		EpollExecutorContext {
			executor: self,
			key,
		}
	}

	fn add_fd(&self, fd: os::unix::io::RawFd, data: *const ()) {
		assert_ne!(fd as u64, u64::max_value());
		self.epoll.add(
			fd,
			nix::sys::epoll::EpollFlags::EPOLLIN
				| nix::sys::epoll::EpollFlags::EPOLLOUT
				| nix::sys::epoll::EpollFlags::EPOLLRDHUP
				| nix::sys::epoll::EpollFlags::EPOLLERR
				| nix::sys::epoll::EpollFlags::EPOLLHUP
				| nix::sys::epoll::EpollFlags::EPOLLET,
			data,
		);
	}

	fn remove_fd(&self, fd: os::unix::io::RawFd, data: *const ()) {
		self.epoll.delete(fd, data);
	}

	fn add_instant(&self, instant: time::Instant, data: *const ()) -> heap::Slot {
		logln!("{:?}: {}: add_instant", nix::unistd::getpid(), ::pid());
		let mut timer = self.timer.write().unwrap();
		let slot = timer.push(TimeEvent(instant, data));
		self.epoll.update_timeout(instant);
		slot
	}

	fn remove_instant(&self, slot: heap::Slot) {
		self.timer.write().unwrap().remove(slot);
	}

	fn add_trigger(&self, data: *const ()) -> (Triggerer, Triggeree) {
		let eventfd = nix::sys::eventfd::eventfd(
			0,
			nix::sys::eventfd::EfdFlags::EFD_CLOEXEC | nix::sys::eventfd::EfdFlags::EFD_NONBLOCK,
		).unwrap();
		assert_eq!(
			nix::unistd::read(eventfd, unsafe {
				mem::transmute::<&mut u64, &mut [u8; 8]>(&mut 0)
			}),
			Err(nix::Error::Sys(nix::errno::Errno::EAGAIN))
		);
		self.epoll.add(
			eventfd,
			nix::sys::epoll::EpollFlags::EPOLLIN
				| nix::sys::epoll::EpollFlags::EPOLLRDHUP
				| nix::sys::epoll::EpollFlags::EPOLLERR
				| nix::sys::epoll::EpollFlags::EPOLLHUP
				| nix::sys::epoll::EpollFlags::EPOLLET,
			data,
		);
		(Triggerer { eventfd }, Triggeree { eventfd })
	}

	pub fn wait<
		F: FnMut(&EpollExecutor, Either<nix::sys::epoll::EpollFlags, time::Instant>, *const ()),
	>(
		&self, mut f: F,
	) {
		let mut done_any = false;
		let now = time::Instant::now();
		let timeout = {
			loop {
				let TimeEvent(timeout, epoll_key) = {
					let timer = &mut *self.timer.write().unwrap();
					if timer.peek().is_some() && &timer.peek().unwrap().0 <= &now {
						logln!(
							"{:?}: {}: timeout unelapsed {:?} <= {:?}",
							nix::unistd::getpid(),
							::pid(),
							timer.peek().unwrap().0,
							now
						);
					}
					if timer.peek().is_none() || &timer.peek().unwrap().0 > &now {
						break;
					}
					timer.pop().unwrap()
				};
				done_any = true;
				logln!(
					"{:?}: {}: ran timeout {:?}",
					nix::unistd::getpid(),
					::pid(),
					timeout
				);
				f(self, Either::Right(timeout), epoll_key)
			}
			self.timer.read().unwrap().peek().map(|x| x.0)
		};
		logln!(
			"{:?}: {}: \\wait_all {:?}",
			nix::unistd::getpid(),
			::pid(),
			timeout
		);
		if let Some(timeout) = timeout {
			self.epoll.update_timeout(timeout);
		}
		self.epoll.wait_all(done_any, |_epoll, flags, epoll_key| {
			f(self, Either::Left(flags), epoll_key)
		});
		logln!("{:?}: {}: /wait_all", nix::unistd::getpid(), ::pid());
		let now = time::Instant::now();
		loop {
			let TimeEvent(timeout, epoll_key) = {
				let timer = &mut *self.timer.write().unwrap();
				if timer.peek().is_some() && &timer.peek().unwrap().0 <= &now {
					logln!(
						"{:?}: {}: timeout unelapsed {:?} <= {:?}",
						nix::unistd::getpid(),
						::pid(),
						timer.peek().unwrap().0,
						now
					);
				}
				if timer.peek().is_none() || &timer.peek().unwrap().0 > &now {
					break;
				}
				timer.pop().unwrap()
			};
			logln!(
				"{:?}: {}: ran timeout {:?}",
				nix::unistd::getpid(),
				::pid(),
				timeout
			);
			f(self, Either::Right(timeout), epoll_key)
		}
	}
}
pub struct Triggerer {
	// TODO: drop?
	eventfd: os::unix::io::RawFd,
}
impl Triggerer {
	pub fn trigger(&self) {
		nix::unistd::write(self.eventfd, &unsafe { mem::transmute::<u64, [u8; 8]>(1) }).unwrap();
		logln!(
			"{:?}: {}: trigger {}",
			nix::unistd::getpid(),
			::pid(),
			self.eventfd
		);
	}
}
pub struct Triggeree {
	// TODO: drop?
	eventfd: os::unix::io::RawFd,
}
impl Triggeree {
	pub fn triggered(&self) {
		let mut y = 0;
		let x = nix::unistd::read(self.eventfd, unsafe {
			mem::transmute::<&mut u64, &mut [u8; 8]>(&mut y)
		}).unwrap();
		assert_eq!(x, 8);
		assert_eq!(y, 1);
		logln!(
			"{:?}: {}: triggered {}",
			nix::unistd::getpid(),
			::pid(),
			self.eventfd
		);
	}

	pub fn is_triggered(&self) -> bool {
		// for non-epoll only
		nix::unistd::read(self.eventfd, unsafe {
			mem::transmute::<&mut u64, &mut [u8; 8]>(&mut 0)
		}).is_ok()
	}
}
impl ops::Drop for Triggeree {
	fn drop(&mut self) {
		nix::unistd::close(self.eventfd).unwrap()
	}
}

const EPOLL_BUF_LENGTH: usize = 100; // size = 100*16

pub struct Epoll {
	fd: os::unix::io::RawFd,
	timer: os::unix::io::RawFd,
	timeout: sync::Mutex<Option<time::Instant>>,
	strip: sync::Mutex<Option<HashSet<usize>>>,
}
impl Epoll {
	pub fn new() -> Epoll {
		let fd = nix::sys::epoll::epoll_create1(nix::sys::epoll::EpollCreateFlags::EPOLL_CLOEXEC)
			.unwrap();
		let timer = unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_NONBLOCK) };
		assert!(timer != -1);
		nix::sys::epoll::epoll_ctl(
			fd,
			nix::sys::epoll::EpollOp::EpollCtlAdd,
			timer,
			&mut unsafe {
				*(&nix::sys::epoll::EpollEvent::new(
					nix::sys::epoll::EpollFlags::EPOLLIN,
					u64::max_value(),
				) as *const _ as *mut _)
			},
		).unwrap();
		Epoll {
			fd,
			timer,
			timeout: sync::Mutex::new(None),
			strip: sync::Mutex::new(None),
		}
	}

	pub fn add(
		&self, fd: os::unix::io::RawFd, events: nix::sys::epoll::EpollFlags, data: *const (),
	) {
		assert_ne!(data as u64, u64::max_value());
		if let &mut Some(ref mut strip) = &mut *self.strip.lock().unwrap() {
			strip.remove(&(data as usize));
		}
		nix::sys::epoll::epoll_ctl(
			self.fd,
			nix::sys::epoll::EpollOp::EpollCtlAdd,
			fd,
			Some(&mut nix::sys::epoll::EpollEvent::new(events, data as u64)),
		).unwrap();
	}

	pub fn modify(
		&self, fd: os::unix::io::RawFd, events: nix::sys::epoll::EpollFlags, data: *const (),
	) {
		assert_ne!(data as u64, u64::max_value());
		nix::sys::epoll::epoll_ctl(
			self.fd,
			nix::sys::epoll::EpollOp::EpollCtlMod,
			fd,
			Some(&mut nix::sys::epoll::EpollEvent::new(events, data as u64)),
		).unwrap();
	}

	pub fn delete(&self, fd: os::unix::io::RawFd, data: *const ()) {
		nix::sys::epoll::epoll_ctl(self.fd, nix::sys::epoll::EpollOp::EpollCtlDel, fd, None)
			.unwrap();
		if let &mut Some(ref mut strip) = &mut *self.strip.lock().unwrap() {
			let x = strip.insert(data as usize);
			assert!(x);
		}
	}

	pub fn update_timeout(&self, timeout: time::Instant) {
		let mut current_timeout = self.timeout.lock().unwrap();
		logln!(
			"{:?}: {}: Epoll::update_timeout {:?} {:?}",
			nix::unistd::getpid(),
			::pid(),
			current_timeout,
			timeout
		);
		if current_timeout.is_none() || timeout < current_timeout.unwrap() {
			*current_timeout = Some(timeout);
			let err = unsafe {
				libc::timerfd_settime(
					self.timer,
					libc::TFD_TIMER_ABSTIME,
					&libc::itimerspec {
						it_interval: libc::timespec {
							tv_sec: 0,
							tv_nsec: 0,
						},
						it_value: libc::timespec {
							tv_sec: timeout.as_secs() as i64,
							tv_nsec: timeout.subsec_nanos() as i64,
						},
					},
					ptr::null_mut() as *mut libc::itimerspec,
				)
			};
			assert_eq!(err, 0);
		}
	}

	pub fn wait<F: FnMut(&Epoll, nix::sys::epoll::EpollFlags, *const ())>(
		&self, nonblock: bool, mut f: F,
	) {
		// TODO: synchronize
		let mut events: [nix::sys::epoll::EpollEvent; EPOLL_BUF_LENGTH] =
			unsafe { mem::uninitialized() };
		let x = mem::replace(&mut *self.strip.lock().unwrap(), Some(HashSet::new()));
		assert!(x.is_none());
		let n = loop {
			logln!(
				"{:?}: {}: \\epoll_wait {:?}",
				nix::unistd::getpid(),
				::pid(),
				nonblock
			);
			let n =
				nix::sys::epoll::epoll_wait(self.fd, &mut events, if nonblock { 0 } else { -1 });
			logln!(
				"{:?}: {}: /epoll_wait: {:?}",
				nix::unistd::getpid(),
				::pid(),
				n
			);
			if let Err(nix::Error::Sys(nix::errno::Errno::EINTR)) = n {
				continue;
			}
			let n = n.unwrap();
			if !nonblock && n == 0 {
				continue;
			}
			let mut current_timeout = self.timeout.lock().unwrap();
			if let Ok(_) = nix::unistd::read(self.timer, unsafe {
				mem::transmute::<&mut u64, &mut [u8; 8]>(&mut 0)
			}) {
				*current_timeout = None;
			}
			break n;
		};
		assert!(n <= events.len());
		// if let Timeout::Time(timeout) = timeout {
		// assert!((&events[..n]).into_iter().filter(|x|x.data()==u64::max_value()).count() <= 1 && ((&events[..n]).into_iter().filter(|x|x.data()!=u64::max_value()).count() > 0 || timeout <= time::Instant::now()));
		// } else if let Timeout::Never = timeout {
		// 	assert!((&events[..n]).into_iter().filter(|x|x.data()==u64::max_value()).count() == 0 && (&events[..n]).into_iter().filter(|x|x.data()!=u64::max_value()).count() > 0);
		// }
		let strip = mem::replace(&mut *self.strip.lock().unwrap(), None).unwrap(); // TODO: what about strips added after this point
		for x in (&mut events[..n])
			.into_iter()
			.filter(|x| x.data() != u64::max_value() && !strip.contains(&(x.data() as usize)))
		{
			f(self, x.events(), x.data() as *const ())
		}
	}

	pub fn wait_all<F: FnMut(&Epoll, nix::sys::epoll::EpollFlags, *const ())>(
		&self, mut nonblock: bool, mut f: F,
	) {
		// TODO: synchronize
		let mut events: [nix::sys::epoll::EpollEvent; EPOLL_BUF_LENGTH] =
			unsafe { mem::uninitialized() };
		loop {
			let x = mem::replace(&mut *self.strip.lock().unwrap(), Some(HashSet::new()));
			assert!(x.is_none());
			let n = loop {
				logln!(
					"{:?}: {}: \\epoll_wait {:?}",
					nix::unistd::getpid(),
					::pid(),
					nonblock
				);
				let n = nix::sys::epoll::epoll_wait(
					self.fd,
					&mut events,
					if nonblock { 0 } else { -1 },
				);
				logln!(
					"{:?}: {}: /epoll_wait: {:?}",
					nix::unistd::getpid(),
					::pid(),
					n
				);
				if let Err(nix::Error::Sys(nix::errno::Errno::EINTR)) = n {
					continue;
				}
				let n = n.unwrap();
				if !nonblock && n == 0 {
					continue;
				}
				let mut current_timeout = self.timeout.lock().unwrap();
				if let Ok(_) = nix::unistd::read(self.timer, unsafe {
					mem::transmute::<&mut u64, &mut [u8; 8]>(&mut 0)
				}) {
					*current_timeout = None;
				}
				break n;
			};
			assert!(n <= events.len());
			// if let Timeout::Time(timeout) = timeout {
			// assert!((&events[..n]).into_iter().filter(|x|x.data()==u64::max_value()).count() <= 1 && ((&events[..n]).into_iter().filter(|x|x.data()!=u64::max_value()).count() > 0 || timeout <= time::Instant::now()));
			// } else if let Timeout::Never = timeout {
			// 	assert!((&events[..n]).into_iter().filter(|x|x.data()==u64::max_value()).count() == 0 && (&events[..n]).into_iter().filter(|x|x.data()!=u64::max_value()).count() > 0);
			// }
			let strip = mem::replace(&mut *self.strip.lock().unwrap(), None).unwrap(); // TODO: what about strips added after this point
			for x in (&mut events[..n])
				.into_iter()
				.filter(|x| x.data() != u64::max_value() && !strip.contains(&(x.data() as usize)))
			{
				f(self, x.events(), x.data() as *const ())
			}
			if n < events.len() {
				break;
			}
			nonblock = true;
		}
	}
}
impl ops::Drop for Epoll {
	fn drop(&mut self) {
		nix::unistd::close(self.timer).unwrap();
		nix::unistd::close(self.fd).unwrap();
	}
}

#[derive(Copy, Clone)]
struct Timespec {
	t: libc::timespec,
}
#[derive(Copy, Clone)]
struct Instant {
	t: Timespec,
}
impl fmt::Debug for Instant {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Instant")
			.field("tv_sec", &self.t.t.tv_sec)
			.field("tv_nsec", &self.t.t.tv_nsec)
			.finish()
	}
}
impl Publicise for time::Instant {
	type Public = Instant;

	fn publicise(&self) -> &Self::Public {
		let b = unsafe { mem::transmute(self) };
		assert_eq!(format!("{:?}", self), format!("{:?}", b));
		b
	}
}
trait Publicise {
	type Public;
	fn publicise(&self) -> &Self::Public;
}
trait X {
	fn as_secs(&self) -> u64;
	fn subsec_nanos(&self) -> u32;
}
impl X for time::Instant {
	fn as_secs(&self) -> u64 {
		self.publicise().t.t.tv_sec as u64
	}

	fn subsec_nanos(&self) -> u32 {
		self.publicise().t.t.tv_nsec as u32
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

const LISTEN_BACKLOG: usize = 128; // 1

#[derive(Debug)]
pub struct LinuxTcpListener {
	fd: os::unix::io::RawFd,
	xxx: bool,
}
impl LinuxTcpListener {
	pub fn new_ephemeral(
		host: &net::IpAddr, executor: &EpollExecutorContext,
	) -> (LinuxTcpListener, u16) {
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
				&net::SocketAddr::new(host.clone(), 0),
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
		executor.add_fd(process_listener);
		nix::sys::socket::listen(process_listener, LISTEN_BACKLOG).unwrap();
		(
			LinuxTcpListener {
				fd: process_listener,
				xxx: false,
			},
			process_id,
		)
	}

	pub fn with_fd(
		process_listener: os::unix::io::RawFd, executor: &EpollExecutorContext,
	) -> LinuxTcpListener {
		executor.add_fd(process_listener);
		nix::sys::socket::listen(process_listener, LISTEN_BACKLOG).unwrap();
		LinuxTcpListener {
			fd: process_listener,
			xxx: false,
		}
	}

	pub fn into_fd(self) -> os::unix::io::RawFd {
		let ret = self.fd;
		mem::forget(self);
		ret
	}

	pub fn with_socket_forwardee(
		socket_forwardee: SocketForwardee, executor: &EpollExecutorContext,
	) -> LinuxTcpListener {
		executor.add_fd(socket_forwardee.0);
		LinuxTcpListener {
			fd: socket_forwardee.0,
			xxx: true,
		}
	}

	pub fn poll<'a, F: FnMut(&os::unix::io::RawFd) -> Option<SocketForwarder>>(
		&'a mut self, executor: &'a EpollExecutorContext, accept_hook: &'a mut F,
	) -> impl Iterator<
		Item = (
			LinuxTcp,
			net::SocketAddr,
			fn(&EpollExecutorContext, &LinuxTcp),
		),
	>
	          + 'a {
		itertools::unfold((), move |_| {
			loop {
				let fd = if !self.xxx {
					nix::sys::socket::accept4(
						self.fd,
						nix::sys::socket::SockFlag::SOCK_CLOEXEC
							| nix::sys::socket::SockFlag::SOCK_NONBLOCK,
					)
				} else {
					SocketForwardee(self.fd).recv()
				};
				match fd {
					Ok(fd) => {
						if nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::AcceptConn)
							.unwrap()
						{
							logln!(
								"{}:{}: %%% got sent listener!",
								nix::unistd::getpid(),
								nix::unistd::gettid()
							);
							assert!(self.xxx);
							executor.remove_fd(self.fd);
							nix::unistd::close(self.fd).unwrap();
							assert!(
								nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL).unwrap()
									& nix::fcntl::OFlag::O_NONBLOCK.bits() != 0
							);
							executor.add_fd(fd);
							self.fd = fd;
							self.xxx = false;
						} else {
							match accept_hook(&fd) {
								None => {
									if let (Ok(remote), 0) = (
										nix::sys::socket::getpeername(fd),
										nix::sys::socket::getsockopt(
											fd,
											nix::sys::socket::sockopt::SocketError,
										).unwrap(),
									) {
										let remote = if let nix::sys::socket::SockAddr::Inet(inet) =
											remote
										{
											inet.to_std()
										} else {
											panic!()
										};
										// logln!("{:?}: {}: accepted {:?} {:?}", time::SystemTime::now().duration_since(time::UNIX_EPOCH).unwrap(), ::pid(), local, remote);
										nix::sys::socket::setsockopt(
											fd,
											nix::sys::socket::sockopt::ReusePort,
											&true,
										).unwrap();
										nix::sys::socket::setsockopt(
											fd,
											nix::sys::socket::sockopt::ReuseAddr,
											&true,
										).unwrap();
										nix::sys::socket::setsockopt(
											fd,
											nix::sys::socket::sockopt::Linger,
											&libc::linger {
												l_onoff: 1,
												l_linger: 10,
											},
										).unwrap(); // assert that close is quick?? https://www.nybek.com/blog/2015/04/29/so_linger-on-non-blocking-sockets/
										nix::sys::socket::setsockopt(
											fd,
											nix::sys::socket::sockopt::TcpNoDelay,
											&true,
										).unwrap();
										logln!(
											"{:?}: {}: accepted1",
											time::SystemTime::now()
												.duration_since(time::UNIX_EPOCH)
												.unwrap(),
											::pid()
										); //, remote);
										let connectee = LinuxTcpConnectee::new(
											fd,
											time::Instant::now()
												+ time::Duration::new(CONNECT_TIMEOUT, 0),
											&EpollExecutorContext {
												executor: executor.executor,
												key: u64::max_value() as *const (),
											},
											remote,
										); // TODO!!!
										if let Either::Right(None) = connectee {
											// nix::unistd::close(fd).unwrap();
											logln!(
												"{:?}: {}: !!accepted",
												time::SystemTime::now()
													.duration_since(time::UNIX_EPOCH)
													.unwrap(),
												::pid()
											); //, remote);
										} else {
											let connectee =
												connectee.map_right(|right| right.unwrap());
											let connectee = match connectee {
												Either::Left(connectee) => {
													LinuxTcp::Connectee(connectee)
												}
												Either::Right(Either::Left(connected)) => {
													LinuxTcp::Connected(connected)
												}
												Either::Right(Either::Right(remote_closed)) => {
													LinuxTcp::RemoteClosed(remote_closed)
												}
											};
											return Some((
												connectee,
												remote,
												(|executor: &EpollExecutorContext,
												  connectee: &LinuxTcp| {
													executor.add_instant(time::Instant::now());
													executor.add_fd(match connectee {
														&LinuxTcp::Connectee(ref connectee) => {
															connectee.fd
														}
														&LinuxTcp::Connected(ref connected) => {
															connected.fd
														}
														&LinuxTcp::RemoteClosed(
															ref remote_closed,
														) => remote_closed.fd,
														_ => unreachable!(),
													});
												})
													as fn(&EpollExecutorContext, &LinuxTcp),
											));
										}
									} else {
										nix::unistd::close(fd).unwrap();
										logln!(
											"{:?}: {}: !accepted",
											time::SystemTime::now()
												.duration_since(time::UNIX_EPOCH)
												.unwrap(),
											::pid()
										); //, remote);
									}
								}
								Some(to) => {
									to.send(fd);
								}
							}
						}
					}
					Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => return None,
					Err(err) => panic!(
						"{}:{}: {:?}, {:?}, {:?}, {:?}",
						nix::unistd::getpid(),
						nix::unistd::gettid(),
						err,
						self.xxx,
						nix::errno::errno(),
						self.fd
					),
				}
			}
		})
	}
}
impl ops::Drop for LinuxTcpListener {
	fn drop(&mut self) {
		logln!("{}: drop LinuxTcpListener", ::pid());
		// executor.remove_fd(self.fd); TODO
		nix::unistd::close(self.fd).unwrap();
	}
}

#[derive(Clone)]
pub struct SocketForwarder(os::unix::io::RawFd);
pub struct SocketForwardee(os::unix::io::RawFd);
pub fn socket_forwarder() -> (SocketForwarder, SocketForwardee) {
	let (send, receive) = os::unix::net::UnixDatagram::pair().unwrap();
	receive.set_nonblocking(true).unwrap();
	(
		SocketForwarder(send.into_raw_fd()),
		SocketForwardee(receive.into_raw_fd()),
	)
}
impl SocketForwarder {
	pub fn send(&self, fd: os::unix::io::RawFd) -> Result<(), nix::Error> {
		let iov = [nix::sys::uio::IoVec::from_slice(&[])];
		let arr = [fd];
		let cmsg = [nix::sys::socket::ControlMessage::ScmRights(&arr)];
		nix::sys::socket::sendmsg(
			self.0,
			&iov,
			&cmsg,
			nix::sys::socket::MsgFlags::empty(),
			None,
		).map(|x| assert_eq!(x, 0))
	}
}
impl SocketForwardee {
	pub fn recv(&self) -> Result<os::unix::io::RawFd, nix::Error> {
		let mut buf = [0u8; 8];
		let iovec = [nix::sys::uio::IoVec::from_mut_slice(&mut buf)];
		let mut space = nix::sys::socket::CmsgSpace::<[os::unix::io::RawFd; 2]>::new();
		nix::sys::socket::recvmsg(
			self.0,
			&iovec,
			Some(&mut space),
			nix::sys::socket::MsgFlags::MSG_DONTWAIT,
		).map(|msg| {
			let mut iter = msg.cmsgs();
			match iter.next() {
				Some(nix::sys::socket::ControlMessage::ScmRights(fds)) => {
					assert_eq!(msg.bytes, 0);
					assert_eq!(fds.len(), 1);
					fds[0]
				}
				Some(e) => panic!("{:?}", mem::discriminant(&e)),
				None => panic!("None"),
			}
		})
	}
}

#[derive(Debug)]
pub enum LinuxTcp {
	Connecter(LinuxTcpConnecter),
	Connectee(LinuxTcpConnectee),
	ConnecterLocalClosed(LinuxTcpConnecterLocalClosed),
	ConnecteeLocalClosed(LinuxTcpConnecteeLocalClosed),
	Connected(LinuxTcpConnection),
	RemoteClosed(LinuxTcpConnectionRemoteClosed),
	LocalClosed(LinuxTcpConnectionLocalClosed),
	Closing(LinuxTcpClosing),
	Closed,
	Killed,
	Xxx,
}
impl LinuxTcp {
	pub fn connect(
		local: net::SocketAddr, remote: net::SocketAddr, executor: &EpollExecutorContext,
	) -> Self {
		match LinuxTcpConnecter::new(local, remote, executor) {
			Either::Left(connecter) => LinuxTcp::Connecter(connecter),
			Either::Right(Some(Either::Left(connected))) => LinuxTcp::Connected(connected),
			Either::Right(Some(Either::Right(remote_closed))) => {
				LinuxTcp::RemoteClosed(remote_closed)
			}
			Either::Right(None) => LinuxTcp::Killed,
		}
	}

	pub fn poll(&mut self, executor: &EpollExecutorContext) {
		*self = match mem::replace(self, LinuxTcp::Xxx) {
			LinuxTcp::Connecter(connecter) => match connecter.poll(executor) {
				Either::Left(connecter) => LinuxTcp::Connecter(connecter),
				Either::Right(Some(Either::Left(connected))) => LinuxTcp::Connected(connected),
				Either::Right(Some(Either::Right(remote_closed))) => {
					LinuxTcp::RemoteClosed(remote_closed)
				}
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::Connectee(connectee) => match connectee.poll(executor) {
				Either::Left(connectee) => LinuxTcp::Connectee(connectee),
				Either::Right(Some(Either::Left(connected))) => LinuxTcp::Connected(connected),
				Either::Right(Some(Either::Right(remote_closed))) => {
					LinuxTcp::RemoteClosed(remote_closed)
				}
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::ConnecterLocalClosed(connected_local_closed) => {
				match connected_local_closed.poll(executor) {
					Either::Left(connected_local_closed) => {
						LinuxTcp::ConnecterLocalClosed(connected_local_closed)
					}
					Either::Right(Some(Either::Left(local_closed))) => {
						LinuxTcp::LocalClosed(local_closed)
					}
					Either::Right(Some(Either::Right(Either::Left(closing)))) => {
						LinuxTcp::Closing(closing)
					}
					Either::Right(Some(Either::Right(Either::Right(())))) => LinuxTcp::Closed,
					Either::Right(None) => LinuxTcp::Killed,
				}
			}
			LinuxTcp::ConnecteeLocalClosed(connectee_local_closed) => {
				match connectee_local_closed.poll(executor) {
					Either::Left(connectee_local_closed) => {
						LinuxTcp::ConnecteeLocalClosed(connectee_local_closed)
					}
					Either::Right(Some(Either::Left(local_closed))) => {
						LinuxTcp::LocalClosed(local_closed)
					}
					Either::Right(Some(Either::Right(Either::Left(closing)))) => {
						LinuxTcp::Closing(closing)
					}
					Either::Right(Some(Either::Right(Either::Right(())))) => LinuxTcp::Closed,
					Either::Right(None) => LinuxTcp::Killed,
				}
			}
			LinuxTcp::Connected(connected) => match connected.poll(executor) {
				Either::Left(connected) => LinuxTcp::Connected(connected),
				Either::Right(Some(remote_closed)) => LinuxTcp::RemoteClosed(remote_closed),
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::RemoteClosed(remote_closed) => match remote_closed.poll(executor) {
				Some(remote_closed) => LinuxTcp::RemoteClosed(remote_closed),
				None => LinuxTcp::Killed,
			},
			LinuxTcp::LocalClosed(local_closed) => match local_closed.poll(executor) {
				Either::Left(local_closed) => LinuxTcp::LocalClosed(local_closed),
				Either::Right(Some(Either::Left(closing))) => LinuxTcp::Closing(closing),
				Either::Right(Some(Either::Right(()))) => LinuxTcp::Closed,
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::Closing(closing) => match closing.poll(executor) {
				Either::Left(closing) => LinuxTcp::Closing(closing),
				Either::Right(Some(())) => LinuxTcp::Closed,
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::Closed => LinuxTcp::Closed,
			LinuxTcp::Killed => LinuxTcp::Killed,
			LinuxTcp::Xxx => unreachable!(),
		};
	}

	pub fn connecting(&self) -> bool {
		match self {
			&LinuxTcp::Connecter(_) => true,
			&LinuxTcp::Connectee(_) => true,
			&LinuxTcp::ConnecterLocalClosed(_) => true,
			&LinuxTcp::ConnecteeLocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn recvable(&self) -> bool {
		match self {
			&LinuxTcp::Connected(_) => true,
			&LinuxTcp::LocalClosed(_) => true,
			_ => false,
		}
	}

	pub fn recv_avail(&self) -> bool {
		match self {
			&LinuxTcp::Connected(ref connected) => connected.recv_avail(),
			&LinuxTcp::LocalClosed(ref local_closed) => local_closed.recv_avail(),
			_ => false,
		}
	}

	pub fn recv(&mut self, executor: &EpollExecutorContext) -> u8 {
		match self {
			&mut LinuxTcp::Connected(ref mut connected) => connected.recv(executor),
			&mut LinuxTcp::LocalClosed(ref mut local_closed) => local_closed.recv(executor),
			_ => panic!(),
		}
	}

	pub fn sendable(&self) -> bool {
		match self {
			&LinuxTcp::Connected(_) => true,
			&LinuxTcp::RemoteClosed(_) => true,
			_ => false,
		}
	}

	pub fn send_avail(&self) -> bool {
		match self {
			&LinuxTcp::Connected(ref connected) => connected.send_avail(),
			&LinuxTcp::RemoteClosed(ref remote_closed) => remote_closed.send_avail(),
			_ => false,
		}
	}

	pub fn send(&mut self, x: u8, executor: &EpollExecutorContext) {
		match self {
			&mut LinuxTcp::Connected(ref mut connected) => connected.send(x, executor),
			&mut LinuxTcp::RemoteClosed(ref mut remote_closed) => remote_closed.send(x, executor),
			_ => panic!(),
		}
	}

	pub fn closable(&self) -> bool {
		match self {
			&LinuxTcp::Connecter(_) => true,
			&LinuxTcp::Connectee(_) => true,
			&LinuxTcp::Connected(_) => true,
			&LinuxTcp::RemoteClosed(_) => true,
			_ => false,
		}
	}

	pub fn closed(&self) -> bool {
		match self {
			&LinuxTcp::Closed => true,
			_ => false,
		}
	}

	pub fn valid(&self) -> bool {
		match self {
			&LinuxTcp::Connecter(_) => true,
			&LinuxTcp::Connectee(_) => true,
			&LinuxTcp::ConnecterLocalClosed(_) => true,
			&LinuxTcp::ConnecteeLocalClosed(_) => true,
			&LinuxTcp::Connected(_) => true,
			&LinuxTcp::RemoteClosed(_) => true,
			&LinuxTcp::LocalClosed(_) => true,
			&LinuxTcp::Closing(_) => true,
			&LinuxTcp::Closed => true,
			&LinuxTcp::Killed => false,
			&LinuxTcp::Xxx => unreachable!(),
		}
	}

	pub fn close(&mut self, executor: &EpollExecutorContext) {
		*self = match mem::replace(self, LinuxTcp::Xxx) {
			LinuxTcp::Connecter(connecter) => match connecter.close(executor) {
				Either::Left(connecter_local_closed) => {
					LinuxTcp::ConnecterLocalClosed(connecter_local_closed)
				}
				Either::Right(Some(Either::Left(local_closed))) => {
					LinuxTcp::LocalClosed(local_closed)
				}
				Either::Right(Some(Either::Right(Either::Left(closing)))) => {
					LinuxTcp::Closing(closing)
				}
				Either::Right(Some(Either::Right(Either::Right(())))) => LinuxTcp::Closed,
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::Connectee(connectee) => match connectee.close(executor) {
				Either::Left(connectee_local_closed) => {
					LinuxTcp::ConnecteeLocalClosed(connectee_local_closed)
				}
				Either::Right(Some(Either::Left(local_closed))) => {
					LinuxTcp::LocalClosed(local_closed)
				}
				Either::Right(Some(Either::Right(Either::Left(closing)))) => {
					LinuxTcp::Closing(closing)
				}
				Either::Right(Some(Either::Right(Either::Right(())))) => LinuxTcp::Closed,
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::Connected(connected) => match connected.close(executor) {
				Either::Left(local_closed) => LinuxTcp::LocalClosed(local_closed),
				Either::Right(Some(Either::Left(closing))) => LinuxTcp::Closing(closing),
				Either::Right(Some(Either::Right(()))) => LinuxTcp::Closed,
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::RemoteClosed(remote_closed) => match remote_closed.close(executor) {
				Either::Left(closing) => LinuxTcp::Closing(closing),
				Either::Right(Some(())) => LinuxTcp::Closed,
				Either::Right(None) => LinuxTcp::Killed,
			},
			LinuxTcp::LocalClosed(_local_closed) => panic!(),
			LinuxTcp::ConnecterLocalClosed(_connecter_local_closed) => panic!(),
			LinuxTcp::ConnecteeLocalClosed(_connectee_local_closed) => panic!(),
			LinuxTcp::Closing(_closing) => panic!(),
			LinuxTcp::Closed => panic!(),
			LinuxTcp::Killed => panic!(),
			LinuxTcp::Xxx => unreachable!(),
		};
	}
}

#[derive(PartialEq, Eq, Clone, Debug)]
enum LinuxTcpConnecterState {
	Init,
	Connecting1(os::unix::io::RawFd, time::Instant),
	Connecting2(os::unix::io::RawFd, time::Instant),
}
#[derive(Debug)]
pub struct LinuxTcpConnecter {
	state: LinuxTcpConnecterState,
	local: net::SocketAddr,
	remote: net::SocketAddr,
}
unsafe impl Send for LinuxTcpConnecter {}
unsafe impl Sync for LinuxTcpConnecter {} // because *const ()
impl LinuxTcpConnecter {
	pub fn new(
		local: net::SocketAddr, remote: net::SocketAddr, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<LinuxTcpConnection, LinuxTcpConnectionRemoteClosed>>> {
		logln!(
			"{:?}: {}: connect {}",
			time::SystemTime::now()
				.duration_since(time::UNIX_EPOCH)
				.unwrap(),
			::pid(),
			::Pid::new(remote.ip(), remote.port())
		);
		LinuxTcpConnecter {
			state: LinuxTcpConnecterState::Init,
			local,
			remote,
		}.poll(executor)
	}

	pub fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<LinuxTcpConnection, LinuxTcpConnectionRemoteClosed>>> {
		let mut count = 0;
		loop {
			count += 1;
			assert!(count < 1_000);
			match self.state {
				LinuxTcpConnecterState::Init => {
					let fd = nix::sys::socket::socket(
						nix::sys::socket::AddressFamily::Inet,
						nix::sys::socket::SockType::Stream,
						nix::sys::socket::SockFlag::SOCK_CLOEXEC
							| nix::sys::socket::SockFlag::SOCK_NONBLOCK,
						nix::sys::socket::SockProtocol::Tcp,
					).unwrap();
					nix::sys::socket::setsockopt(fd, nix::sys::socket::sockopt::ReusePort, &true)
						.unwrap();
					nix::sys::socket::setsockopt(fd, nix::sys::socket::sockopt::ReuseAddr, &true)
						.unwrap();
					nix::sys::socket::setsockopt(
						fd,
						nix::sys::socket::sockopt::Linger,
						&libc::linger {
							l_onoff: 1,
							l_linger: 10,
						},
					).unwrap();
					nix::sys::socket::setsockopt(fd, nix::sys::socket::sockopt::TcpNoDelay, &true)
						.unwrap();
					nix::sys::socket::bind(
						fd,
						&nix::sys::socket::SockAddr::Inet(nix::sys::socket::InetAddr::from_std(
							&self.local,
						)),
					).unwrap();
					executor.add_fd(fd);
					logln!(
						"{:?}: {}: connecting {}",
						time::SystemTime::now()
							.duration_since(time::UNIX_EPOCH)
							.unwrap(),
						::pid(),
						::Pid::new(self.remote.ip(), self.remote.port())
					);
					if match nix::sys::socket::connect(
						fd,
						&nix::sys::socket::SockAddr::Inet(nix::sys::socket::InetAddr::from_std(
							&self.remote,
						)),
					) {
						Err(nix::Error::Sys(nix::errno::Errno::EINPROGRESS)) => true,
						Err(nix::Error::Sys(nix::errno::Errno::EADDRNOTAVAIL)) => {
							/*logln!("EADDRNOTAVAIL");*/
							false
						}
						Err(nix::Error::Sys(nix::errno::Errno::ECONNABORTED)) => {
							logln!("ECONNABORTED");
							false
						}
						err => panic!("{:?}", err),
					}
						&& nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError)
							.unwrap() == 0
					{
						// sometimes ECONNRESET; sometimes ECONNREFUSED (after remote segfaulted?)
						logln!(
							"{:?}: {}: connecting1 {}",
							time::SystemTime::now()
								.duration_since(time::UNIX_EPOCH)
								.unwrap(),
							::pid(),
							::Pid::new(self.remote.ip(), self.remote.port())
						);
						self.state = LinuxTcpConnecterState::Connecting1(
							fd,
							time::Instant::now() + time::Duration::new(CONNECT_TIMEOUT, 0),
						);
					} else {
						executor.remove_fd(fd);
						nix::unistd::close(fd).unwrap();
						let timeout = time::Instant::now() + time::Duration::new(0, 100_000);
						logln!(
							"{:?}: {}: queue timeout {:?}",
							nix::unistd::getpid(),
							::pid(),
							timeout
						);
						executor.add_instant(timeout);
						return Either::Left(self);
					}
				}
				LinuxTcpConnecterState::Connecting1(fd, timeout) => {
					if nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError)
						.unwrap() == 0
					{
						let mut events =
							[nix::poll::PollFd::new(fd, nix::poll::EventFlags::POLLOUT)];
						let n = nix::poll::poll(&mut events, 0).unwrap();
						assert!(n == 0 || n == 1);
						if n == 1 && events[0].revents().unwrap() == nix::poll::EventFlags::POLLOUT
						{
							match nix::sys::socket::send(
								fd,
								&unsafe { mem::transmute::<u64, [u8; 8]>(OUTGOING_MAGIC) },
								nix::sys::socket::MsgFlags::MSG_DONTWAIT
									| nix::sys::socket::MsgFlags::from_bits(
										0, /*libc::MSG_NOSIGNAL*/
									).unwrap(),
							) {
								Ok(8) => {
									self.state = LinuxTcpConnecterState::Connecting2(fd, timeout);
								}
								Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => unimplemented!(),
								Err(nix::Error::Sys(nix::errno::Errno::ECONNRESET)) => {
									executor.remove_fd(fd);
									nix::unistd::close(fd).unwrap();
									self.state = LinuxTcpConnecterState::Init;
								}
								err => panic!("AAA: {:?}", err),
							}
						} else if timeout < time::Instant::now() {
							logln!("timedout a");
							executor.remove_fd(fd);
							nix::unistd::close(fd).unwrap();
							self.state = LinuxTcpConnecterState::Init;
						} else {
							assert_ne!(self.state, LinuxTcpConnecterState::Init);
							return Either::Left(self);
						}
					} else {
						executor.remove_fd(fd);
						nix::unistd::close(fd).unwrap();
						self.state = LinuxTcpConnecterState::Init;
					}
				}
				LinuxTcpConnecterState::Connecting2(fd, timeout) => {
					let x =
						nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError)
							.unwrap();
					if x == 0 {
						let mut magic: u64 = 0;
						match nix::sys::socket::recv(
							fd,
							unsafe { mem::transmute::<&mut u64, &mut [u8; 8]>(&mut magic) },
							nix::sys::socket::MsgFlags::MSG_PEEK
								| nix::sys::socket::MsgFlags::MSG_DONTWAIT
								| nix::sys::socket::MsgFlags::from_bits(
									0, /*libc::MSG_NOSIGNAL*/
								).unwrap(),
						) {
							Ok(8) => {
								if magic == INCOMING_MAGIC {
									let x = nix::sys::socket::recv(
										fd,
										unsafe { mem::transmute::<&mut u64, &mut [u8; 8]>(&mut 0) },
										nix::sys::socket::MsgFlags::MSG_DONTWAIT
											| nix::sys::socket::MsgFlags::from_bits(
												0, /*libc::MSG_NOSIGNAL*/
											).unwrap(),
									).unwrap();
									assert_eq!(x, 8);
									logln!(
										"{:?}: {}: connected {}",
										time::SystemTime::now()
											.duration_since(time::UNIX_EPOCH)
											.unwrap(),
										::pid(),
										::Pid::new(self.remote.ip(), self.remote.port())
									);
									let ret = Either::Right(match LinuxTcpConnection::new(
										fd,
										executor,
										self.remote,
									) {
										Either::Left(x) => Some(Either::Left(x)),
										Either::Right(Some(x)) => Some(Either::Right(x)),
										Either::Right(None) => None,
									});
									mem::forget(self);
									return ret;
								} else {
									logln!("{}: bad magic B {:?}", ::pid(), magic);
									panic!();
									executor.remove_fd(fd);
									nix::unistd::close(fd).unwrap();
									self.state = LinuxTcpConnecterState::Init;
								}
							}
							Ok(0) | Err(nix::Error::Sys(nix::errno::Errno::ECONNRESET)) => {
								executor.remove_fd(fd);
								logln!(
									"{}: Ok(0)/ECONNRESET {}",
									::pid(),
									::Pid::new(self.remote.ip(), self.remote.port())
								);
								nix::unistd::close(fd).unwrap();
								self.state = LinuxTcpConnecterState::Init;
							}
							Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => {
								if timeout < time::Instant::now() {
									logln!("timedout b");
									self.state = LinuxTcpConnecterState::Connecting2(
										fd,
										timeout + time::Duration::new(CONNECT_TIMEOUT, 0),
									);
								// nix::unistd::close(fd).unwrap();
								// self.state = LinuxTcpConnecterState::Init;
								} else {
									assert_ne!(self.state, LinuxTcpConnecterState::Init);
									return Either::Left(self);
								}
							}
							err => panic!("{:?}", err),
						}
					} else {
						logln!(
							"{}: socketerr {} {:?}",
							::pid(),
							::Pid::new(self.remote.ip(), self.remote.port()),
							nix::errno::Errno::from_i32(x)
						);
						executor.remove_fd(fd);
						nix::unistd::close(fd).unwrap();
						self.state = LinuxTcpConnecterState::Init;
					}
				}
			}
		}
	}

	pub fn close(
		self, executor: &EpollExecutorContext,
	) -> Either<
		LinuxTcpConnecterLocalClosed,
		Option<Either<LinuxTcpConnectionLocalClosed, Either<LinuxTcpClosing, ()>>>,
	> {
		let ret = LinuxTcpConnecterLocalClosed::new(
			self.state.clone(),
			self.local,
			self.remote,
			executor,
		);
		mem::forget(self);
		ret
	}
}
impl ops::Drop for LinuxTcpConnecter {
	fn drop(&mut self) {
		match self.state {
			LinuxTcpConnecterState::Connecting1(fd, _)
			| LinuxTcpConnecterState::Connecting2(fd, _) => {
				// executor.remove_fd(fd); // TODO
				nix::unistd::close(fd).unwrap();
			}
			_ => (),
		}
	}
}

#[derive(Debug)]
pub struct LinuxTcpConnectee {
	fd: os::unix::io::RawFd,
	timeout: time::Instant,
	sent: bool,
	remote: net::SocketAddr,
}
impl LinuxTcpConnectee {
	fn new(
		fd: os::unix::io::RawFd, timeout: time::Instant, executor: &EpollExecutorContext,
		remote: net::SocketAddr,
	) -> Either<Self, Option<Either<LinuxTcpConnection, LinuxTcpConnectionRemoteClosed>>> {
		LinuxTcpConnectee {
			fd,
			timeout,
			sent: false,
			remote,
		}.poll(executor)
	}

	pub fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<LinuxTcpConnection, LinuxTcpConnectionRemoteClosed>>> {
		let x =
			nix::sys::socket::getsockopt(self.fd, nix::sys::socket::sockopt::SocketError).unwrap();
		if x == 0 {
			if !self.sent {
				match nix::sys::socket::send(
					self.fd,
					&unsafe { mem::transmute::<u64, [u8; 8]>(INCOMING_MAGIC) },
					nix::sys::socket::MsgFlags::MSG_DONTWAIT
						| nix::sys::socket::MsgFlags::from_bits(0 /*libc::MSG_NOSIGNAL*/).unwrap(),
				) {
					Ok(8) => self.sent = true,
					Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => unimplemented!(),
					Err(nix::Error::Sys(nix::errno::Errno::ECONNRESET)) => {
						return Either::Right(None);
					}
					err => panic!("AAA: {:?}", err),
				}
			}
			if self.sent {
				let mut magic: u64 = 0;
				match nix::sys::socket::recv(
					self.fd,
					unsafe { mem::transmute::<&mut u64, &mut [u8; 8]>(&mut magic) },
					nix::sys::socket::MsgFlags::MSG_PEEK
						| nix::sys::socket::MsgFlags::MSG_DONTWAIT
						| nix::sys::socket::MsgFlags::from_bits(0 /*libc::MSG_NOSIGNAL*/).unwrap(),
				) {
					Ok(8) => {
						if magic == OUTGOING_MAGIC {
							let x = nix::sys::socket::recv(
								self.fd,
								unsafe { mem::transmute::<&mut u64, &mut [u8; 8]>(&mut 0) },
								nix::sys::socket::MsgFlags::MSG_DONTWAIT
									| nix::sys::socket::MsgFlags::from_bits(
										0, /*libc::MSG_NOSIGNAL*/
									).unwrap(),
							).unwrap();
							assert_eq!(x, 8);
							logln!(
								"{:?}: {}: accepted {}",
								time::SystemTime::now()
									.duration_since(time::UNIX_EPOCH)
									.unwrap(),
								::pid(),
								::Pid::new(self.remote.ip(), self.remote.port())
							);
							let ret = Either::Right(match LinuxTcpConnection::new(
								self.fd,
								executor,
								self.remote,
							) {
								Either::Left(x) => Some(Either::Left(x)),
								Either::Right(Some(x)) => Some(Either::Right(x)),
								Either::Right(None) => None,
							});
							mem::forget(self);
							ret
						} else {
							logln!("bad magic A {:?}", magic);
							panic!();
							Either::Right(None)
						}
					}
					Ok(0) | Err(nix::Error::Sys(nix::errno::Errno::ECONNRESET)) => {
						logln!("{}: Ok(0)/ECONNRESET", ::pid()); //, ::Pid::new(self.remote.ip(),self.remote.port()));
						Either::Right(None)
					}
					Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => {
						if self.timeout < time::Instant::now() {
							logln!("timedout c");
							self.timeout += time::Duration::new(CONNECT_TIMEOUT, 0);
							Either::Left(self)
						// Either::Right(None)
						} else {
							Either::Left(self)
						}
					}
					err => panic!("{:?}", err),
				}
			} else {
				Either::Left(self)
			}
		} else {
			logln!(
				"{}: LinuxTcpConnectee err: **{:?}** {}",
				::pid(),
				nix::errno::Errno::from_i32(x),
				::Pid::new(self.remote.ip(), self.remote.port())
			);
			Either::Right(None)
		}
	}

	pub fn close(
		self, executor: &EpollExecutorContext,
	) -> Either<
		LinuxTcpConnecteeLocalClosed,
		Option<Either<LinuxTcpConnectionLocalClosed, Either<LinuxTcpClosing, ()>>>,
	> {
		let ret = LinuxTcpConnecteeLocalClosed::new(
			self.fd,
			self.timeout,
			self.sent,
			executor,
			self.remote,
		);
		mem::forget(self);
		ret
	}
}
impl ops::Drop for LinuxTcpConnectee {
	fn drop(&mut self) {
		// executor.remove_fd(self.fd); // TODO
		nix::unistd::close(self.fd).unwrap();
	}
}

#[derive(Debug)]
pub struct LinuxTcpConnecterLocalClosed {
	state: LinuxTcpConnecterState,
	local: net::SocketAddr,
	remote: net::SocketAddr,
}
unsafe impl Send for LinuxTcpConnecterLocalClosed {}
unsafe impl Sync for LinuxTcpConnecterLocalClosed {} // because *const ()
impl LinuxTcpConnecterLocalClosed {
	fn new(
		state: LinuxTcpConnecterState, local: net::SocketAddr, remote: net::SocketAddr,
		executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<LinuxTcpConnectionLocalClosed, Either<LinuxTcpClosing, ()>>>> {
		LinuxTcpConnecterLocalClosed {
			state,
			local,
			remote,
		}.poll(executor)
	}

	pub fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<LinuxTcpConnectionLocalClosed, Either<LinuxTcpClosing, ()>>>> {
		let mut count = 0;
		loop {
			count += 1;
			assert!(count < 1_000);
			match self.state {
				LinuxTcpConnecterState::Init => {
					mem::forget(self);
					return Either::Right(Some(Either::Right(Either::Right(()))));
				}
				LinuxTcpConnecterState::Connecting1(fd, timeout) => {
					if nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError)
						.unwrap() == 0
					{
						let mut events =
							[nix::poll::PollFd::new(fd, nix::poll::EventFlags::POLLOUT)];
						let n = nix::poll::poll(&mut events, 0).unwrap();
						assert!(n == 0 || n == 1);
						if n == 1 && events[0].revents().unwrap() == nix::poll::EventFlags::POLLOUT
						{
							match nix::sys::socket::send(
								fd,
								&unsafe { mem::transmute::<u64, [u8; 8]>(OUTGOING_MAGIC) },
								nix::sys::socket::MsgFlags::MSG_DONTWAIT
									| nix::sys::socket::MsgFlags::from_bits(
										0, /*libc::MSG_NOSIGNAL*/
									).unwrap(),
							) {
								Ok(8) => {
									self.state = LinuxTcpConnecterState::Connecting2(fd, timeout);
								}
								Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => unimplemented!(),
								Err(nix::Error::Sys(nix::errno::Errno::ECONNRESET)) => {
									executor.remove_fd(fd);
									nix::unistd::close(fd).unwrap();
									self.state = LinuxTcpConnecterState::Init;
								}
								err => panic!("AAA: {:?}", err),
							}
						} else if timeout < time::Instant::now() {
							logln!("timedout a");
							executor.remove_fd(fd);
							nix::unistd::close(fd).unwrap();
							self.state = LinuxTcpConnecterState::Init;
						} else {
							assert_ne!(self.state, LinuxTcpConnecterState::Init);
							return Either::Left(self);
						}
					} else {
						executor.remove_fd(fd);
						nix::unistd::close(fd).unwrap();
						self.state = LinuxTcpConnecterState::Init;
					}
				}
				LinuxTcpConnecterState::Connecting2(fd, timeout) => {
					let x =
						nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError)
							.unwrap();
					if x == 0 {
						let mut magic: u64 = 0;
						match nix::sys::socket::recv(
							fd,
							unsafe { mem::transmute::<&mut u64, &mut [u8; 8]>(&mut magic) },
							nix::sys::socket::MsgFlags::MSG_PEEK
								| nix::sys::socket::MsgFlags::MSG_DONTWAIT
								| nix::sys::socket::MsgFlags::from_bits(
									0, /*libc::MSG_NOSIGNAL*/
								).unwrap(),
						) {
							Ok(8) => {
								if magic == INCOMING_MAGIC {
									let x = nix::sys::socket::recv(
										fd,
										unsafe { mem::transmute::<&mut u64, &mut [u8; 8]>(&mut 0) },
										nix::sys::socket::MsgFlags::MSG_DONTWAIT
											| nix::sys::socket::MsgFlags::from_bits(
												0, /*libc::MSG_NOSIGNAL*/
											).unwrap(),
									).unwrap();
									assert_eq!(x, 8);
									logln!(
										"{:?}: {}: connected {}",
										time::SystemTime::now()
											.duration_since(time::UNIX_EPOCH)
											.unwrap(),
										::pid(),
										::Pid::new(self.remote.ip(), self.remote.port())
									);
									let ret =
										Either::Right(match LinuxTcpConnectionLocalClosed::new(
											fd,
											CircularBuffer::new(BUF),
											CircularBuffer::new(BUF),
											false,
											executor,
											self.remote,
										) {
											Either::Left(x) => Some(Either::Left(x)),
											Either::Right(Some(x)) => Some(Either::Right(x)),
											Either::Right(None) => None,
										});
									mem::forget(self);
									return ret;
								} else {
									logln!("{}: bad magic B {:?}", ::pid(), magic);
									panic!();
									executor.remove_fd(fd);
									nix::unistd::close(fd).unwrap();
									self.state = LinuxTcpConnecterState::Init;
								}
							}
							Ok(0) | Err(nix::Error::Sys(nix::errno::Errno::ECONNRESET)) => {
								executor.remove_fd(fd);
								logln!(
									"{}: Ok(0)/ECONNRESET {}",
									::pid(),
									::Pid::new(self.remote.ip(), self.remote.port())
								);
								nix::unistd::close(fd).unwrap();
								self.state = LinuxTcpConnecterState::Init;
							}
							Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => {
								if timeout < time::Instant::now() {
									logln!("timedout b");
									self.state = LinuxTcpConnecterState::Connecting2(
										fd,
										timeout + time::Duration::new(CONNECT_TIMEOUT, 0),
									);
								// nix::unistd::close(fd).unwrap();
								// self.state = LinuxTcpConnecterState::Init;
								} else {
									assert_ne!(self.state, LinuxTcpConnecterState::Init);
									return Either::Left(self);
								}
							}
							err => panic!("{:?}", err),
						}
					} else {
						logln!(
							"{}: socketerr {} {:?}",
							::pid(),
							::Pid::new(self.remote.ip(), self.remote.port()),
							nix::errno::Errno::from_i32(x)
						);
						executor.remove_fd(fd);
						nix::unistd::close(fd).unwrap();
						self.state = LinuxTcpConnecterState::Init;
					}
				}
			}
		}
	}
}
impl ops::Drop for LinuxTcpConnecterLocalClosed {
	fn drop(&mut self) {
		match self.state {
			LinuxTcpConnecterState::Connecting1(fd, _)
			| LinuxTcpConnecterState::Connecting2(fd, _) => {
				// executor.remove_fd(fd); // TODO
				nix::unistd::close(fd).unwrap();
			}
			_ => (),
		}
	}
}

#[derive(Debug)]
pub struct LinuxTcpConnecteeLocalClosed {
	// TODO
	fd: os::unix::io::RawFd,
	timeout: time::Instant,
	sent: bool,
	remote: net::SocketAddr,
}
impl LinuxTcpConnecteeLocalClosed {
	fn new(
		fd: os::unix::io::RawFd, timeout: time::Instant, sent: bool,
		executor: &EpollExecutorContext, remote: net::SocketAddr,
	) -> Either<Self, Option<Either<LinuxTcpConnectionLocalClosed, Either<LinuxTcpClosing, ()>>>> {
		logln!(
			"{}: {}:{}: LinuxTcpConnecteeLocalClosed::new {:?} ({})",
			::pid(),
			nix::unistd::getpid(),
			nix::unistd::gettid(),
			nix::sys::socket::getpeername(fd).map(|remote| {
				if let nix::sys::socket::SockAddr::Inet(inet) = remote {
					inet.to_std()
				} else {
					panic!()
				}
			}),
			fd
		);
		LinuxTcpConnecteeLocalClosed {
			fd,
			timeout,
			sent,
			remote,
		}.poll(executor)
	}

	pub fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<LinuxTcpConnectionLocalClosed, Either<LinuxTcpClosing, ()>>>> {
		let x =
			nix::sys::socket::getsockopt(self.fd, nix::sys::socket::sockopt::SocketError).unwrap();
		if x == 0 {
			if !self.sent {
				match nix::sys::socket::send(
					self.fd,
					&unsafe { mem::transmute::<u64, [u8; 8]>(INCOMING_MAGIC) },
					nix::sys::socket::MsgFlags::MSG_DONTWAIT
						| nix::sys::socket::MsgFlags::from_bits(0 /*libc::MSG_NOSIGNAL*/).unwrap(),
				) {
					Ok(8) => self.sent = true,
					Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => unimplemented!(),
					Err(nix::Error::Sys(nix::errno::Errno::ECONNRESET)) => {
						return Either::Right(None);
					}
					err => panic!("AAA: {:?}", err),
				}
			}
			if self.sent {
				let mut magic: u64 = 0;
				match nix::sys::socket::recv(
					self.fd,
					unsafe { mem::transmute::<&mut u64, &mut [u8; 8]>(&mut magic) },
					nix::sys::socket::MsgFlags::MSG_PEEK
						| nix::sys::socket::MsgFlags::MSG_DONTWAIT
						| nix::sys::socket::MsgFlags::from_bits(0 /*libc::MSG_NOSIGNAL*/).unwrap(),
				) {
					Ok(8) => {
						if magic == OUTGOING_MAGIC {
							let x = nix::sys::socket::recv(
								self.fd,
								unsafe { mem::transmute::<&mut u64, &mut [u8; 8]>(&mut 0) },
								nix::sys::socket::MsgFlags::MSG_DONTWAIT
									| nix::sys::socket::MsgFlags::from_bits(
										0, /*libc::MSG_NOSIGNAL*/
									).unwrap(),
							).unwrap();
							assert_eq!(x, 8);
							logln!(
								"{:?}: {}: accepted {}",
								time::SystemTime::now()
									.duration_since(time::UNIX_EPOCH)
									.unwrap(),
								::pid(),
								::Pid::new(self.remote.ip(), self.remote.port())
							);
							let ret = Either::Right(match LinuxTcpConnectionLocalClosed::new(
								self.fd,
								CircularBuffer::new(BUF),
								CircularBuffer::new(BUF),
								false,
								executor,
								self.remote,
							) {
								Either::Left(x) => Some(Either::Left(x)),
								Either::Right(Some(x)) => Some(Either::Right(x)),
								Either::Right(None) => None,
							});
							mem::forget(self);
							ret
						} else {
							logln!("bad magic A {:?}", magic);
							panic!();
							Either::Right(None)
						}
					}
					Ok(0) | Err(nix::Error::Sys(nix::errno::Errno::ECONNRESET)) => {
						logln!("{}: Ok(0)/ECONNRESET", ::pid()); //, ::Pid::new(self.remote.ip(),self.remote.port()));
						Either::Right(None)
					}
					Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => {
						if self.timeout < time::Instant::now() {
							logln!("timedout c");
							self.timeout += time::Duration::new(CONNECT_TIMEOUT, 0);
							Either::Left(self)
						// Either::Right(None)
						} else {
							Either::Left(self)
						}
					}
					err => panic!("{:?}", err),
				}
			} else {
				Either::Left(self)
			}
		} else {
			logln!(
				"{}: LinuxTcpConnectee err: **{:?}** {}",
				::pid(),
				nix::errno::Errno::from_i32(x),
				::Pid::new(self.remote.ip(), self.remote.port())
			);
			Either::Right(None)
		}
	}
}
impl ops::Drop for LinuxTcpConnecteeLocalClosed {
	fn drop(&mut self) {
		// executor.remove_fd(self.fd); // TODO
		nix::unistd::close(self.fd).unwrap();
	}
}

pub const BUF: usize = 64 * 1024; // 7,8,9,80,99

#[derive(Debug)]
pub struct LinuxTcpConnection {
	fd: os::unix::io::RawFd,
	send: CircularBuffer<u8>,
	recv: CircularBuffer<u8>,
	remote_closed: bool,
	remote: net::SocketAddr,
}
impl LinuxTcpConnection {
	fn new(
		fd: os::unix::io::RawFd, executor: &EpollExecutorContext, remote: net::SocketAddr,
	) -> Either<Self, Option<LinuxTcpConnectionRemoteClosed>> {
		LinuxTcpConnection {
			fd,
			send: CircularBuffer::new(BUF),
			recv: CircularBuffer::new(BUF),
			remote_closed: false,
			remote,
		}.poll(executor)
	}

	pub fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<LinuxTcpConnectionRemoteClosed>> {
		// logln!("LinuxTcpConnection poll");
		let x =
			nix::sys::socket::getsockopt(self.fd, nix::sys::socket::sockopt::SocketError).unwrap();
		if x != 0 {
			logln!(
				"{}: LinuxTcpConnection err: **{:?}** {}",
				::pid(),
				nix::errno::Errno::from_i32(x),
				::Pid::new(self.remote.ip(), self.remote.port())
			);
			return Either::Right(None);
		}
		match self.send.read_to_fd(self.fd) {
			CircularBufferReadToFd::Written(_written) => (),
			CircularBufferReadToFd::Killed => return Either::Right(None),
		}
		if !self.remote_closed {
			match self.recv.write_from_fd(self.fd) {
				CircularBufferWriteFromFd::Read(_read, false) => (),
				CircularBufferWriteFromFd::Read(_read, true) => {
					logln!(
						"{}: LinuxTcpConnection got closed {}",
						::pid(),
						::Pid::new(self.remote.ip(), self.remote.port())
					);
					self.remote_closed = true;
				}
				CircularBufferWriteFromFd::Killed => return Either::Right(None),
			}
		}
		if !self.remote_closed || self.recv.read_available() > 0 {
			Either::Left(self)
		} else {
			let ret = Either::Right(LinuxTcpConnectionRemoteClosed::new(
				self.fd,
				unsafe { ptr::read(&self.send) },
				executor,
				self.remote,
			));
			unsafe { ptr::drop_in_place(&mut self.recv) };
			mem::forget(self);
			ret
		}
	}

	pub fn recv_avail(&self) -> bool {
		self.recv.read_available() > 0
	}

	pub fn recv(&mut self, executor: &EpollExecutorContext) -> u8 {
		let mut byte = [0];
		self.recv.read(&mut byte);
		// self.poll(executor);
		byte[0]
	}

	pub fn send_avail(&self) -> bool {
		self.send.write_available() > 0
	}

	pub fn send(&mut self, x: u8, executor: &EpollExecutorContext) {
		self.send.write(&[x]);
		// self.poll(executor);
	}

	pub fn close(
		self, executor: &EpollExecutorContext,
	) -> Either<LinuxTcpConnectionLocalClosed, Option<Either<LinuxTcpClosing, ()>>> {
		// TODO: simple return type, don't poll
		let ret = LinuxTcpConnectionLocalClosed::new(
			self.fd,
			unsafe { ptr::read(&self.send) },
			unsafe { ptr::read(&self.recv) },
			self.remote_closed,
			executor,
			self.remote,
		);
		mem::forget(self);
		ret
	}
}
impl ops::Drop for LinuxTcpConnection {
	fn drop(&mut self) {
		// executor.remove_fd(self.fd); // TODO
		nix::unistd::close(self.fd).unwrap();
	}
}

#[derive(Debug)]
pub struct LinuxTcpConnectionRemoteClosed {
	fd: os::unix::io::RawFd,
	send: CircularBuffer<u8>,
	remote: net::SocketAddr,
}
impl LinuxTcpConnectionRemoteClosed {
	fn new(
		fd: os::unix::io::RawFd, send: CircularBuffer<u8>, executor: &EpollExecutorContext,
		remote: net::SocketAddr,
	) -> Option<Self> {
		logln!(
			"{}: {}:{}: LinuxTcpConnectionRemoteClosed::new {:?} ({})",
			::pid(),
			nix::unistd::getpid(),
			nix::unistd::gettid(),
			nix::sys::socket::getpeername(fd).map(|remote| {
				if let nix::sys::socket::SockAddr::Inet(inet) = remote {
					inet.to_std()
				} else {
					panic!()
				}
			}),
			fd
		);
		LinuxTcpConnectionRemoteClosed { fd, send, remote }.poll(executor)
	}

	pub fn poll(mut self, _executor: &EpollExecutorContext) -> Option<Self> {
		let mut available: libc::c_int = 0;
		let err = unsafe { libc::ioctl(self.fd, libc::FIONREAD, &mut available) };
		assert!(err == 0 && available >= 0);
		let available = available as usize;
		assert_eq!(available, 0);
		let x =
			nix::sys::socket::getsockopt(self.fd, nix::sys::socket::sockopt::SocketError).unwrap();
		if x != 0 {
			logln!(
				"{}: LinuxTcpConnectionRemoteClosed err: **{:?}** {}",
				::pid(),
				nix::errno::Errno::from_i32(x),
				::Pid::new(self.remote.ip(), self.remote.port())
			); // TODO: for some reason this is hit with EPIPE
			return None;
		}
		match self.send.read_to_fd(self.fd) {
			CircularBufferReadToFd::Written(_written) => Some(self),
			CircularBufferReadToFd::Killed => None,
		}
	}

	pub fn send_avail(&self) -> bool {
		self.send.write_available() > 0
	}

	pub fn send(&mut self, x: u8, executor: &EpollExecutorContext) {
		self.send.write(&[x]);
		// self.poll(executor);
	}

	pub fn close(self, executor: &EpollExecutorContext) -> Either<LinuxTcpClosing, Option<()>> {
		// TODO: simple return type, don't poll
		let ret = LinuxTcpClosing::new(
			self.fd,
			unsafe { ptr::read(&self.send) },
			false,
			executor,
			self.remote,
		);
		mem::forget(self);
		ret
	}
}
impl ops::Drop for LinuxTcpConnectionRemoteClosed {
	fn drop(&mut self) {
		// executor.remove_fd(self.fd); // TODO
		nix::unistd::close(self.fd).unwrap();
	}
}

#[derive(Debug)]
pub struct LinuxTcpConnectionLocalClosed {
	fd: os::unix::io::RawFd,
	send: CircularBuffer<u8>,
	recv: CircularBuffer<u8>,
	remote_closed: bool,
	local_closed_given: bool,
	remote: net::SocketAddr,
}
impl LinuxTcpConnectionLocalClosed {
	fn new(
		fd: os::unix::io::RawFd, send: CircularBuffer<u8>, recv: CircularBuffer<u8>,
		remote_closed: bool, executor: &EpollExecutorContext, remote: net::SocketAddr,
	) -> Either<Self, Option<Either<LinuxTcpClosing, ()>>> {
		LinuxTcpConnectionLocalClosed {
			fd,
			send,
			recv,
			remote_closed,
			local_closed_given: false,
			remote,
		}.poll(executor)
	}

	pub fn poll(
		mut self, executor: &EpollExecutorContext,
	) -> Either<Self, Option<Either<LinuxTcpClosing, ()>>> {
		let x =
			nix::sys::socket::getsockopt(self.fd, nix::sys::socket::sockopt::SocketError).unwrap();
		if x != 0 {
			logln!(
				"{}: LinuxTcpConnectionLocalClosed err: **{:?}** {}",
				::pid(),
				nix::errno::Errno::from_i32(x),
				::Pid::new(self.remote.ip(), self.remote.port())
			);
			return Either::Right(None);
		}
		if !self.local_closed_given {
			match self.send.read_to_fd(self.fd) {
				CircularBufferReadToFd::Written(_written) => (),
				CircularBufferReadToFd::Killed => return Either::Right(None),
			}
		}
		if !self.remote_closed {
			match self.recv.write_from_fd(self.fd) {
				CircularBufferWriteFromFd::Read(_read, false) => (),
				CircularBufferWriteFromFd::Read(_read, true) => {
					logln!(
						"{}: LinuxTcpConnectionLocalClosed got closed {}",
						::pid(),
						::Pid::new(self.remote.ip(), self.remote.port())
					);
					self.remote_closed = true;
				}
				CircularBufferWriteFromFd::Killed => return Either::Right(None),
			}
		}
		if !self.local_closed_given && self.send.read_available() == 0 {
			match nix::sys::socket::shutdown(self.fd, nix::sys::socket::Shutdown::Write) {
				Ok(()) => self.local_closed_given = true,
				Err(err) => {
					logln!("{:?}", err);
					return Either::Right(None);
				}
			}
		}
		if !self.remote_closed || self.recv.read_available() > 0 {
			Either::Left(self)
		} else {
			let ret = Either::Right(match LinuxTcpClosing::new(
				self.fd,
				unsafe { ptr::read(&self.send) },
				self.local_closed_given,
				executor,
				self.remote,
			) {
				Either::Left(x) => Some(Either::Left(x)),
				Either::Right(Some(x)) => Some(Either::Right(x)),
				Either::Right(None) => None,
			});
			unsafe { ptr::drop_in_place(&mut self.recv) };
			mem::forget(self);
			ret
		}
	}

	pub fn recv_avail(&self) -> bool {
		self.recv.read_available() > 0
	}

	pub fn recv(&mut self, executor: &EpollExecutorContext) -> u8 {
		let mut byte = [0];
		self.recv.read(&mut byte);
		// self.poll(executor);
		byte[0]
	}
}
// impl io::Read for LinuxTcpConnectionLocalClosed {
// 	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { io::Read::read(&mut self.recv, buf) }
// }
impl ops::Drop for LinuxTcpConnectionLocalClosed {
	fn drop(&mut self) {
		// executor.remove_fd(self.fd); // TODO
		nix::unistd::close(self.fd).unwrap();
	}
}

#[derive(Debug)]
pub struct LinuxTcpClosing {
	fd: os::unix::io::RawFd,
	send: CircularBuffer<u8>,
	local_closed_given: bool,
	remote: net::SocketAddr,
}
impl LinuxTcpClosing {
	fn new(
		fd: os::unix::io::RawFd, send: CircularBuffer<u8>, local_closed_given: bool,
		executor: &EpollExecutorContext, remote: net::SocketAddr,
	) -> Either<Self, Option<()>> {
		LinuxTcpClosing {
			fd,
			send,
			local_closed_given,
			remote,
		}.poll(executor)
	}

	pub fn poll(mut self, executor: &EpollExecutorContext) -> Either<Self, Option<()>> {
		let mut available: libc::c_int = 0;
		let err = unsafe { libc::ioctl(self.fd, libc::FIONREAD, &mut available) };
		assert!(err == 0 && available >= 0);
		let available = available as usize;
		assert_eq!(available, 0);
		let x =
			nix::sys::socket::getsockopt(self.fd, nix::sys::socket::sockopt::SocketError).unwrap();
		if x != 0 {
			logln!(
				"{}: LinuxTcpClosing err: **{:?}** {}",
				::pid(),
				nix::errno::Errno::from_i32(x),
				::Pid::new(self.remote.ip(), self.remote.port())
			);
			return Either::Right(None);
		}
		match self.send.read_to_fd(self.fd) {
			CircularBufferReadToFd::Written(_written) => (),
			CircularBufferReadToFd::Killed => return Either::Right(None),
		}
		if !self.local_closed_given && self.send.read_available() == 0 {
			match nix::sys::socket::shutdown(self.fd, nix::sys::socket::Shutdown::Write) {
				Ok(()) => self.local_closed_given = true,
				Err(err) => {
					logln!(
						"{}: LinuxTcpClosing shutdown err {:?} {}",
						::pid(),
						err,
						::Pid::new(self.remote.ip(), self.remote.port())
					);
					return Either::Right(None);
				}
			}
		}
		if self.local_closed_given {
			let unsent = {
				let mut unsent: libc::c_int = 0;
				let err = unsafe { libc::ioctl(self.fd, libc::TIOCOUTQ, &mut unsent) };
				assert!(err == 0 && unsent >= 0);
				unsent as usize
			};
			if unsent == 0 {
				unsafe { ptr::drop_in_place(&mut self.send) };
				logln!(
					"{}: LinuxTcpClosing close {}",
					::pid(),
					::Pid::new(self.remote.ip(), self.remote.port())
				);
				executor.remove_fd(self.fd);
				nix::unistd::close(self.fd).unwrap();
				mem::forget(self);
				return Either::Right(Some(()));
			} else {
				executor.add_instant(time::Instant::now() + time::Duration::new(0, 1_000_000));
			}
		}
		Either::Left(self)
	}
}
impl ops::Drop for LinuxTcpClosing {
	fn drop(&mut self) {
		// executor.remove_fd(self.fd); // TODO
		nix::unistd::close(self.fd).unwrap();
	}
}
