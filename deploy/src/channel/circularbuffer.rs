use nix::{self, libc};
use std::{cmp, fmt, io, mem, os, ptr};

#[derive(Clone)]
pub struct CircularBuffer<T> {
	head: usize,
	tail: usize,
	buf: Vec<T>,
}

impl<T> CircularBuffer<T> {
	pub fn new(size: usize) -> CircularBuffer<T> {
		let mut buf = Vec::with_capacity(size);
		unsafe { buf.set_len(size) }; // buf[..].uninit();
		CircularBuffer {
			head: 0,
			tail: 0,
			buf,
		}
	}

	#[inline(always)]
	pub fn len(&self) -> usize {
		self.buf.len()
	}

	#[inline(always)]
	pub fn ptr_mut(&mut self) -> *mut T {
		self.buf.as_mut_ptr()
	}
}
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum CircularBufferReadToFd {
	Written(usize),
	Killed,
}
impl CircularBufferReadToFd {
	#[inline(always)]
	pub fn written(&self) -> usize {
		if let CircularBufferReadToFd::Written(written) = *self {
			written
		} else {
			panic!("CircularBufferReadToFd::Killed::written() called")
		}
	}

	#[inline(always)]
	pub fn killed(&self) -> bool {
		if let CircularBufferReadToFd::Killed = *self {
			true
		} else {
			false
		}
	}
}
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum CircularBufferWriteFromFd {
	Read(usize, bool),
	Killed,
}
impl CircularBufferWriteFromFd {
	#[inline(always)]
	pub fn read(&self) -> (usize, bool) {
		if let CircularBufferWriteFromFd::Read(read, closed) = *self {
			(read, closed)
		} else {
			panic!("CircularBufferWriteFromFd::Killed::Read() called")
		}
	}

	#[inline(always)]
	pub fn killed(&self) -> bool {
		if let CircularBufferWriteFromFd::Killed = *self {
			true
		} else {
			false
		}
	}
}
impl<T> Readable for CircularBuffer<T> {
	type Type = T;

	#[inline(always)]
	fn read_available(&self) -> usize {
		self.head - self.tail
	}

	#[inline(always)]
	fn read_peek(&self, offset: usize, buf: &mut [Self::Type]) {
		assert!((self.tail + offset) + buf.len() <= self.head);
		let a_start = (self.tail + offset) % self.buf.len();
		let a_end = cmp::min(self.buf.len(), a_start + buf.len());
		let b_start = 0;
		let b_end = buf.len() - (a_end - a_start);
		assert!((a_end - a_start) + (b_end - b_start) == buf.len());
		if a_start < a_end {
			unsafe { ptr::copy_nonoverlapping(&self.buf[a_start], &mut buf[0], a_end - a_start) };
		}
		if b_start < b_end {
			unsafe {
				ptr::copy_nonoverlapping(
					&self.buf[b_start],
					&mut buf[a_end - a_start],
					b_end - b_start,
				)
			};
		}
	}

	#[inline(always)]
	fn read_consume(&mut self, len: usize) {
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
		self.tail += len;
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
	}

	#[inline(always)]
	fn read_unconsume_hack(&mut self, len: usize) {
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
		self.tail -= len;
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
	}
}
impl ReadableFd for CircularBuffer<u8> {
	fn read_to_fd(&mut self, fd: os::unix::io::RawFd) -> CircularBufferReadToFd {
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
		let err = nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError).unwrap();
		if err != 0 {
			logln!(
				"a: **{:?}** {} {:?} {:?}",
				nix::errno::Errno::from_i32(err),
				fd,
				nix::sys::socket::getsockname(fd),
				nix::sys::socket::getpeername(fd)
			);
		}
		let mut written = 0;
		let mut n = loop {
			let len = self.head - self.tail;
			assert!(len <= self.buf.len());
			let a_start = self.tail % self.buf.len();
			let a_end = cmp::min(self.buf.len(), a_start + len);
			let b_start = 0;
			let b_end = len - (a_end - a_start);
			assert!((a_end - a_start) + (b_end - b_start) == len);
			if len == 0 {
				let err = nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError)
					.unwrap();
				break if err != 0 {
					logln!(
						"b: **{:?}** {} {:?} {:?}",
						nix::errno::Errno::from_i32(err),
						fd,
						nix::sys::socket::getsockname(fd),
						nix::sys::socket::getpeername(fd)
					);
					Err(nix::Error::Sys(nix::errno::Errno::from_i32(err)))
				} else {
					Ok(written)
				};
			}
			let n = nix::sys::socket::sendmsg(
				fd,
				&[
					nix::sys::uio::IoVec::from_slice(&self.buf[a_start..a_end]),
					nix::sys::uio::IoVec::from_slice(&self.buf[b_start..b_end]),
				],
				&[],
				nix::sys::socket::MsgFlags::MSG_DONTWAIT
					| nix::sys::socket::MsgFlags::from_bits(0 /*libc::MSG_NOSIGNAL*/).unwrap(),
				None,
			);
			if n == Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) {
				break Ok(written);
			} else if n.is_ok() {
				self.tail += n.unwrap();
				written += n.unwrap();
			} else {
				logln!(
					"c: **{:?}** {} {:?} {:?}",
					n.err().unwrap(),
					fd,
					nix::sys::socket::getsockname(fd),
					nix::sys::socket::getpeername(fd)
				);
				assert!(n.is_err());
				break n;
			}
		};
		if n.is_ok() && (err == libc::ECONNRESET || err == libc::ETIMEDOUT) {
			n = Err(nix::Error::Sys(nix::errno::Errno::from_i32(err)));
		}
		if n.is_ok() {
			assert!(err == 0, "{:?}", err);
		}
		if n.is_err() {
			logln!("CircularBuffer read_to_fd: {:?}", n.err());
			assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
			return CircularBufferReadToFd::Killed;
		}
		let n = n.unwrap();
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
		CircularBufferReadToFd::Written(n)
	}
}
impl<T> Writable for CircularBuffer<T> {
	type Type = T;

	#[inline(always)]
	fn write_available(&self) -> usize {
		self.buf.len() - (self.head - self.tail)
	}

	#[inline(always)]
	fn write_peek(&mut self, offset: usize, buf: &[Self::Type]) {
		assert!((self.head + offset) + buf.len() - self.tail <= self.buf.len());
		let a_start = (self.head + offset) % self.buf.len();
		let a_end = cmp::min(self.buf.len(), a_start + buf.len());
		let b_start = 0;
		let b_end = buf.len() - (a_end - a_start);
		assert!((a_end - a_start) + (b_end - b_start) == buf.len());
		if a_start < a_end {
			unsafe { ptr::copy_nonoverlapping(&buf[0], &mut self.buf[a_start], a_end - a_start) };
		}
		if b_start < b_end {
			unsafe {
				ptr::copy_nonoverlapping(
					&buf[a_end - a_start],
					&mut self.buf[b_start],
					b_end - b_start,
				)
			};
		}
	}

	#[inline(always)]
	fn write_consume(&mut self, len: usize) {
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
		self.head += len;
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
	}
}
impl WritableFd for CircularBuffer<u8> {
	fn write_from_fd(&mut self, fd: os::unix::io::RawFd) -> CircularBufferWriteFromFd {
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
		let err = nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError).unwrap();
		if err != 0 {
			logln!(
				"a: **{:?}** {} {:?} {:?}",
				nix::errno::Errno::from_i32(err),
				fd,
				nix::sys::socket::getsockname(fd),
				nix::sys::socket::getpeername(fd)
			);
		}
		let mut read = 0;
		let mut n = loop {
			let len = self.buf.len() - (self.head - self.tail);
			assert!(len <= self.buf.len());
			let a_start = self.head % self.buf.len();
			let a_end = cmp::min(self.buf.len(), a_start + len);
			let b_start = 0;
			let b_end = len - (a_end - a_start);
			assert!((a_end - a_start) + (b_end - b_start) == len);
			if len == 0 {
				let mut byte = [0u8];
				let n = nix::sys::socket::recvmsg::<()>(
					fd,
					&[nix::sys::uio::IoVec::from_mut_slice(&mut byte[..])],
					None,
					nix::sys::socket::MsgFlags::MSG_DONTWAIT
						| nix::sys::socket::MsgFlags::MSG_PEEK
						| nix::sys::socket::MsgFlags::from_bits(0 /*libc::MSG_NOSIGNAL*/).unwrap(),
				).map(|x| x.bytes);
				break if n == Ok(0) {
					Ok((read, true))
				} else if n == Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) || n == Ok(1) {
					Ok((read, false))
				} else {
					logln!(
						"b: **{:?}** {} {:?} {:?}",
						n.err().unwrap(),
						fd,
						nix::sys::socket::getsockname(fd),
						nix::sys::socket::getpeername(fd)
					);
					assert!(n.is_err());
					n.map(|_| unreachable!())
				};
			}
			let n = nix::sys::socket::recvmsg::<()>(
				fd,
				&[
					nix::sys::uio::IoVec::from_mut_slice(unsafe {
						mem::transmute(&mut self.buf[a_start..a_end])
					}),
					nix::sys::uio::IoVec::from_mut_slice(&mut self.buf[b_start..b_end]),
				],
				None,
				nix::sys::socket::MsgFlags::MSG_DONTWAIT
					| nix::sys::socket::MsgFlags::from_bits(0 /*libc::MSG_NOSIGNAL*/).unwrap(),
			).map(|x| x.bytes);
			if n == Ok(0) {
				break Ok((read, true));
			} else if n == Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) {
				break Ok((read, false));
			} else if n.is_ok() {
				self.head += n.unwrap();
				read += n.unwrap();
			} else {
				logln!(
					"c: **{:?}** {} {:?} {:?}",
					n.err().unwrap(),
					fd,
					nix::sys::socket::getsockname(fd),
					nix::sys::socket::getpeername(fd)
				);
				assert!(n.is_err());
				break n.map(|_| unreachable!());
			}
		};
		if n.is_ok() && (err == libc::ECONNRESET || err == libc::ETIMEDOUT) {
			n = Err(nix::Error::Sys(nix::errno::from_i32(err)));
		}
		if n.is_ok() {
			assert!(err == 0, "{:?}", err);
		}
		if n.is_err() {
			logln!("CircularBuffer write_from_fd: {:?}", n.err());
			assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
			return CircularBufferWriteFromFd::Killed;
		}
		let (n, closed) = n.unwrap();
		assert!(self.tail <= self.head && self.head <= self.tail + self.buf.len());
		CircularBufferWriteFromFd::Read(n, closed)
	}
}

impl<T> fmt::Debug for CircularBuffer<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_tuple("CircularBuffer")
			.field(&self.read_available())
			.field(&self.write_available())
			.finish()
	}
}

impl io::Read for CircularBuffer<u8> {
	#[inline]
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		let amt = cmp::min(buf.len(), self.read_available());
		Readable::read(self, &mut buf[..amt]);
		Ok(amt)
	}
}
impl io::Write for CircularBuffer<u8> {
	#[inline]
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		let amt = cmp::min(buf.len(), self.write_available());
		if buf.len() > 0 && amt == 0 {
			logln!("CircularBuffer::write WouldBlock");
			return Err(io::ErrorKind::WouldBlock.into());
		}
		Writable::write(self, &buf[..amt]);
		logln!("CircularBuffer::write2 {:?}", amt);
		Ok(amt)
	}

	#[inline]
	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}

pub trait Readable {
	type Type;
	#[inline(always)]
	fn read(&mut self, buf: &mut [Self::Type]) {
		self.read_peek(0, buf);
		self.read_consume(buf.len());
	}
	#[inline(always)]
	fn pipe<D: Writable<Type = Self::Type>>(&mut self, to: &mut D, len: usize) {
		assert!(self.read_available() >= len && to.write_available() >= len);
		let mut t = Vec::with_capacity(len);
		unsafe { t.set_len(len) }; // t[..].uninit();
		self.read(&mut *t);
		assert!(t.len() == len);
		assert!(to.write_available() >= len);
		to.write(&t);
	}
	fn read_available(&self) -> usize;
	fn read_peek(&self, offset: usize, buf: &mut [Self::Type]);
	fn read_consume(&mut self, len: usize);
	fn read_unconsume_hack(&mut self, len: usize);
}
pub trait ReadableFd: Readable {
	fn read_to_fd(&mut self, fd: os::unix::io::RawFd) -> CircularBufferReadToFd;
}
pub trait Writable {
	type Type;
	#[inline(always)]
	fn write(&mut self, buf: &[Self::Type]) {
		assert!(self.write_available() >= buf.len());
		self.write_peek(0, buf);
		self.write_consume(buf.len());
	}
	fn write_available(&self) -> usize;
	fn write_peek(&mut self, offset: usize, buf: &[Self::Type]);
	fn write_consume(&mut self, len: usize);
}
pub trait WritableFd: Writable {
	fn write_from_fd(&mut self, fd: os::unix::io::RawFd) -> CircularBufferWriteFromFd;
}
