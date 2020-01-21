use rand::{self, Rng, SeedableRng};
use std::{
	borrow, convert::TryInto, fmt, fs, io::{self, Write}, os::{self, unix::io::IntoRawFd}
};

use super::{DeployOutputEvent, Pid};

const STDOUT: os::unix::io::RawFd = 1;
const STDERR: os::unix::io::RawFd = 2;

#[derive(Debug)]
struct Writer<A: Write, B: Write> {
	// TODO: handle buffering better: write returns a referencing struct that buffers and flushes on drop
	fd: os::unix::io::RawFd,
	stdout: A,
	stderr: B,
}
impl<A: Write, B: Write> Writer<A, B> {
	fn write_fmt(&mut self, fd: os::unix::io::RawFd, args: fmt::Arguments) {
		if self.fd == STDOUT && fd != STDOUT {
			self.stdout.flush().unwrap();
		}
		if self.fd == STDERR && fd != STDERR {
			self.stderr.flush().unwrap();
		}
		match fd {
			STDOUT => self.stdout.write_fmt(args).unwrap(),
			STDERR => self.stderr.write_fmt(args).unwrap(),
			fd => {
				let mut file = io::BufWriter::with_capacity(4096 /*PIPE_BUF*/, unsafe {
					<fs::File as os::unix::io::FromRawFd>::from_raw_fd(fd)
				});
				file.write_fmt(args).unwrap();
				file.flush().unwrap();
				let _ = file.into_inner().unwrap().into_raw_fd();
			}
		}
		self.fd = fd;
	}

	fn write(&mut self, fd: os::unix::io::RawFd, buf: &[u8]) {
		if self.fd == STDOUT && fd != STDOUT {
			self.stdout.flush().unwrap();
		}
		if self.fd == STDERR && fd != STDERR {
			self.stderr.flush().unwrap();
		}
		match fd {
			STDOUT => self.stdout.write_all(buf).unwrap(),
			STDERR => self.stderr.write_all(buf).unwrap(),
			fd => {
				let mut file = unsafe { <fs::File as os::unix::io::FromRawFd>::from_raw_fd(fd) };
				file.write_all(buf).unwrap();
				file.flush().unwrap();
				let _ = file.into_raw_fd();
			}
		}
		self.fd = fd;
	}
}

#[derive(Debug)]
pub struct Formatter<A: Write, B: Write> {
	// TODO: if we get half a multi-byte character/combined thing, then something else, then rest of it, it'll be malformatted. deadline cache?
	writer: Writer<A, B>,
	pid: Pid,
	nl: Option<os::unix::io::RawFd>,
	style_support: StyleSupport,
}
impl<A: Write, B: Write> Formatter<A, B> {
	pub fn new(pid: Pid, style_support: StyleSupport, stdout: A, stderr: B) -> Self {
		eprintln!("{}:", pretty_pid(&pid, true, style_support));
		Self {
			writer: Writer {
				fd: STDERR,
				stdout,
				stderr,
			},
			pid,
			nl: None,
			style_support,
		}
	}

	#[allow(clippy::too_many_lines)]
	pub fn write(&mut self, event: &DeployOutputEvent) {
		match *event {
			DeployOutputEvent::Spawn(pid_, new_pid) => {
				assert_ne!(pid_, new_pid);
				if self.nl.is_some() {
					self.writer.write(STDERR, b"\n");
					self.nl = None;
				}
				if pid_ != self.pid {
					self.pid = pid_;
					self.writer.write_fmt(
						STDERR,
						format_args!("{}:\n", pretty_pid(&self.pid, true, self.style_support)),
					);
				}
				self.writer.write_fmt(
					STDERR,
					format_args!(
						"   {} {}\n",
						self.style_support.style().bold().paint("spawned:"),
						pretty_pid(&new_pid, false, self.style_support)
					),
				);
			}
			DeployOutputEvent::Output(pid_, fd, ref output) => {
				if !output.is_empty() {
					if fd == STDOUT || fd == STDERR {
						if pid_ != self.pid {
							self.pid = pid_;
							if self.nl.is_some() {
								self.writer.write(STDERR, b"\n");
								self.nl = None;
							}
							self.writer.write_fmt(
								STDERR,
								format_args!(
									"{}:\n",
									pretty_pid(&self.pid, true, self.style_support)
								),
							);
						}
						if self.nl.is_some() && self.nl.unwrap() != fd {
							self.writer.write(STDERR, b"\n");
							self.nl = None;
						}
						if self.nl.is_none() {
							self.writer.write(STDERR, b"   ");
						}
						let total_len = output.len();
						let mut output = output.split(|&x| x == b'\n');
						let first = output.next().unwrap();
						self.writer.write(fd, first);
						let mut index = first.len();
						self.nl = Some(fd);
						for output in output {
							self.writer.write(fd, b"\n");
							index += 1;
							if index == total_len {
								assert_eq!(output.len(), 0);
								self.nl = None;
								break;
							}
							self.writer.write(STDERR, b"   ");
							// assert!(!output.is_empty());
							if !output.is_empty() {
								// TODO?
								self.writer.write(fd, output);
								index += output.len();
							}
						}
					} else {
						self.writer.write(fd, &*output);
					}
				} else {
					// TODO: need some form of refcounting??
					// if fd == STDOUT {
					// 	io::stdout().flush().unwrap();
					// } else if fd == STDERR {
					// 	io::stderr().flush().unwrap();
					// }
					// let fd = unsafe{fs::File::from_raw_fd(fd)};
				}
			}
			DeployOutputEvent::Exit(pid_, exit_code_) => {
				if self.nl.is_some() {
					self.writer.write(STDERR, b"\n");
					self.nl = None;
				}
				if pid_ != self.pid {
					self.pid = pid_;
					self.writer.write_fmt(
						STDERR,
						format_args!("{}:\n", pretty_pid(&self.pid, true, self.style_support)),
					);
				}
				if exit_code_.success() {
					self.writer.write_fmt(
						STDERR,
						format_args!("   {}\n", self.style_support.style().bold().paint("exited")),
					);
				} else {
					self.writer.write_fmt(
						STDERR,
						format_args!(
							"   {} {:?}\n",
							self.style_support.style().bold().paint("exited:"),
							exit_code_
						),
					);
				}
				// self.writer.write_fmt(STDERR, format_args!("   {} {:?}\nremaining: {}\n", self.style_support.style().bold().paint("exited:"), exit_code_, std::slice::SliceConcatExt::join(&*xyz.iter().map(|pid|pretty_pid(pid,false).to_string()).collect::<Vec<_>>(), ",")));
			}
		}
	}
}

#[derive(Copy, Clone, Debug)]
pub enum StyleSupport {
	None,
	FourBit,
	EightBit,
	TwentyFourBit,
}
impl StyleSupport {
	pub fn style(&self) -> Style {
		Style(*self, ansi_term::Style::new())
	}
}

#[derive(Clone, Copy, Debug)]
pub struct Style(StyleSupport, ansi_term::Style);
impl Style {
	pub fn color(&self, r: u8, g: u8, b: u8) -> Self {
		match self.0 {
			StyleSupport::None => *self,
			StyleSupport::FourBit => unimplemented!(),
			StyleSupport::EightBit => Self(
				self.0,
				self.1.fg(ansi_term::Colour::Fixed(
					16 + 36 * (r / 43) + 6 * (g / 43) + (b / 43),
				)),
			),
			StyleSupport::TwentyFourBit => Self(self.0, self.1.fg(ansi_term::Colour::RGB(r, g, b))),
		}
	}

	pub fn bold(&self) -> Self {
		match self.0 {
			StyleSupport::None => *self,
			_ => Self(self.0, self.1.bold()),
		}
	}

	pub fn paint<'a, I, S: 'a + ToOwned + ?Sized>(
		self, input: I,
	) -> ansi_term::ANSIGenericString<'a, S>
	where
		I: Into<borrow::Cow<'a, S>>,
		<S as borrow::ToOwned>::Owned: fmt::Debug,
	{
		if let StyleSupport::None = self.0 {
			assert!(self.1.is_plain());
		}
		self.1.paint(input)
	}
}

pub(crate) fn pretty_pid(
	pid: &Pid, bold: bool, style_support: StyleSupport,
) -> impl std::fmt::Display {
	let mut format = pid.format();
	let x = format.clone().collect::<String>();
	let mut rng = rand::rngs::SmallRng::from_seed(
		(0..16)
			.map(|_| format.next().map_or(0, |x| (x as u32).try_into().unwrap()))
			.collect::<Vec<u8>>()[..]
			.try_into()
			.unwrap(),
	);
	let (r, g, b) = loop {
		let (r_, g_, b_): (u8, u8, u8) = rng.gen();
		let (r, g, b) = (u16::from(r_), u16::from(g_), u16::from(b_));
		if (r + r + g + g + g + b) / 6 > 100 {
			// https://stackoverflow.com/questions/596216/formula-to-determine-brightness-of-rgb-color/596241#596241
			break (r_, g_, b_);
		}
	};
	let mut color = style_support.style().color(r, g, b);
	if bold {
		color = color.bold();
	}
	color.paint(x)
}
