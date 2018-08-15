use nix;
use std::{cmp, fmt, os, ptr};

#[derive(Clone)]
pub struct CircularBuffer<T> {
	head: usize,
	tail: usize,
	buf: Vec<T>,
}
impl<T> CircularBuffer<T> {
	pub fn new(cap: usize) -> Self {
		let mut buf = Vec::with_capacity(cap);
		unsafe { buf.set_len(cap) };
		Self {
			head: 0,
			tail: 0,
			buf,
		}
	}
	#[inline(always)]
	pub fn capacity(&self) -> usize {
		self.buf.len()
	}
	#[inline(always)]
	pub fn read_available(&self) -> usize {
		self.head - self.tail
	}
	#[inline(always)]
	pub fn write_available(&self) -> usize {
		self.capacity() - self.read_available()
	}
	#[must_use]
	#[inline(always)]
	pub fn read<'a>(&'a mut self) -> Option<impl FnOnce() -> T + 'a> {
		if self.read_available() > 0 {
			Some(move || {
				let off = self.tail;
				let ret = unsafe { ptr::read(self.buf.get_unchecked(off)) };
				self.tail += 1;
				if self.tail >= self.capacity() {
					self.head -= self.capacity();
					self.tail -= self.capacity();
				}
				ret
			})
		} else {
			None
		}
	}
	#[must_use]
	#[inline(always)]
	pub fn write<'a>(&'a mut self) -> Option<impl FnOnce(T) + 'a> {
		if self.write_available() > 0 {
			Some(move |t| {
				let off = self.head % self.capacity();
				unsafe { ptr::write(self.buf.get_unchecked_mut(off), t) };
				self.head += 1;
			})
		} else {
			None
		}
	}
}
impl<T> Drop for CircularBuffer<T> {
	fn drop(&mut self) {
		while self.read_available() > 0 {
			let _ = self.read();
		}
		unsafe { self.buf.set_len(0) };
	}
}
impl CircularBuffer<u8> {
	pub fn read_to_fd(&mut self, fd: os::unix::io::RawFd) -> Result<usize, nix::Error> {
		let mut written = 0;
		loop {
			if self.read_available() > 0 {
				let a_start = self.tail;
				let a_end = cmp::min(self.capacity(), a_start + self.read_available());
				let b_start = 0;
				let b_end = self.read_available() - (a_end - a_start);
				match nix::sys::socket::sendmsg(
					fd,
					&[
						nix::sys::uio::IoVec::from_slice(&self.buf[a_start..a_end]),
						nix::sys::uio::IoVec::from_slice(&self.buf[b_start..b_end]),
					][0..if b_start != b_end { 2 } else { 1 }],
					&[],
					nix::sys::socket::MsgFlags::empty(),
					None,
				) {
					Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => return Ok(written),
					Ok(n) => {
						self.tail += n;
						if self.tail >= self.capacity() {
							self.head -= self.capacity();
							self.tail -= self.capacity();
						}
						written += n;
					}
					Err(err) => return Err(err),
				}
			} else {
				match nix::sys::socket::getsockopt(fd, nix::sys::socket::sockopt::SocketError)
					.unwrap()
				{
					0 => return Ok(written),
					err => return Err(nix::Error::Sys(nix::errno::Errno::from_i32(err))),
				}
			}
		}
	}
	pub fn write_from_fd(&mut self, fd: os::unix::io::RawFd) -> Result<(usize, bool), nix::Error> {
		let mut read = 0;
		loop {
			if self.write_available() > 0 {
				let a_start = self.head % self.capacity();
				let a_end = cmp::min(self.capacity(), a_start + self.write_available());
				let b_start = 0;
				let b_end = self.write_available() - (a_end - a_start);
				let (b, a) = self.buf.split_at_mut(b_end);
				match nix::sys::socket::recvmsg::<()>(
					fd,
					&[
						nix::sys::uio::IoVec::from_mut_slice(
							&mut a[a_start - b_end..a_end - b_end],
						),
						nix::sys::uio::IoVec::from_mut_slice(b),
					][0..if b_start != b_end { 2 } else { 1 }],
					None,
					nix::sys::socket::MsgFlags::empty(),
				).map(|x| x.bytes)
				{
					Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => return Ok((read, false)),
					Ok(0) => return Ok((read, true)),
					Ok(n) => {
						self.head += n;
						read += n;
					}
					Err(err) => return Err(err),
				}
			} else {
				match nix::sys::socket::recvmsg::<()>(
					fd,
					&[nix::sys::uio::IoVec::from_mut_slice(&mut [0])],
					None,
					nix::sys::socket::MsgFlags::MSG_PEEK,
				).map(|x| x.bytes)
				{
					Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) | Ok(1) => {
						return Ok((read, false))
					}
					Ok(0) => return Ok((read, true)),
					Err(err) => return Err(err),
					Ok(_) => unreachable!(),
				}
			}
		}
	}
}
impl<T> fmt::Debug for CircularBuffer<T> {
	fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
		fmt.debug_tuple("CircularBuffer")
			.field(&format!("{}/{}", self.read_available(), self.capacity()))
			.finish()
	}
}
