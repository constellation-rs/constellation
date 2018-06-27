#![feature(asm)]
#![deny(warnings, deprecated)]

extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate aes_frast;
extern crate ansi_term;
extern crate bincode;
extern crate cargo_metadata as cargo_metadata_;
extern crate either;
extern crate nix;
extern crate rand;
extern crate serde_json;

use rand::{Rng, SeedableRng};
use std::{
	borrow, env, ffi, fmt, fs,
	io::{self, Read, Write},
	mem, net, ops,
	os::{
		self,
		unix::io::{AsRawFd, IntoRawFd},
	},
	path, ptr,
};

/// An opaque identifier for a process.
///
/// The current process's `Pid` can be retrieved with [pid()](pid).
///
/// Unlike typical OS pids, it is:
///  * Universally unique – that is to say, the same `Pid` will never be seen twice
///  * When running across a cluster, it is cluster-wide, rather than within a single instance.
///
/// All inter-process communication occurs after [Sender](Sender)s and [Receiver](Receiver)s have been created with `Pid`s, thus `Pid`s are the sole form of addressing necessary.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Pid([u8; 16]);
impl Pid {
	pub(crate) fn new(ip: net::IpAddr, port: u16) -> Pid {
		match ip {
			net::IpAddr::V4(ip) => {
				let ip = ip.octets();
				Pid([
					ip[0],
					ip[1],
					ip[2],
					ip[3],
					(port >> 8) as u8,
					(port & 0xff) as u8,
					0,
					0,
					0,
					0,
					0,
					0,
					0,
					0,
					0,
					0,
				])
			}
			_ => unimplemented!(),
		}
	}

	pub(crate) fn addr(&self) -> net::SocketAddr {
		net::SocketAddr::new(
			[self.0[0], self.0[1], self.0[2], self.0[3]].into(),
			((self.0[4] as u16) << 8) | (self.0[5] as u16),
		)
	}

	fn format<'a>(&'a self) -> impl Iterator<Item = char> + 'a {
		let key: [u8; 16] = [0; 16];
		encrypt(self.0, key)
			.to_hex()
			.collect::<Vec<_>>()
			.into_iter()
	}
}
impl fmt::Display for Pid {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}", self.format().take(7).collect::<String>())
	}
}
impl fmt::Debug for Pid {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_tuple("Pid")
			.field(&self.format().collect::<String>())
			.finish()
	}
}
pub trait PidInternal {
	fn new(ip: net::IpAddr, port: u16) -> Pid;
	fn addr(&self) -> net::SocketAddr;
}
#[doc(hidden)]
impl PidInternal for Pid {
	fn new(ip: net::IpAddr, port: u16) -> Pid {
		Pid::new(ip, port)
	}

	fn addr(&self) -> net::SocketAddr {
		Pid::addr(self)
	}
}

pub fn parse_binary_size(input: &str) -> Result<u64, ()> {
	let mut index = 0;
	if index == input.len() {
		return Err(());
	}
	index = input
		.chars()
		.position(|c| !c.is_ascii_digit())
		.unwrap_or(input.len());
	let a: u64 = input[..index].parse().unwrap();
	if index == input.len() {
		return Ok(a);
	}
	let b: u64 = if input[index..index + 1].chars().nth(0).unwrap() == '.' {
		index += 1;
		let _zeros = input[index..].chars().position(|c| c != '0').unwrap_or(0);
		let index1 = index;
		index = index
			+ input[index..]
				.chars()
				.position(|c| !c.is_ascii_digit())
				.unwrap_or(input.len() - index);
		if index != index1 {
			input[index1..index].parse().unwrap()
		} else {
			0
		}
	} else {
		0
	};
	if index == input.len() {
		return Ok(a);
	}
	let c: u64 = match &input[index..] {
		"B" => 1,
		"KiB" => 1024,
		"MiB" => 1024u64.pow(2),
		"GiB" => 1024u64.pow(3),
		"TiB" => 1024u64.pow(4),
		"PiB" => 1024u64.pow(5),
		"EiB" => 1024u64.pow(6),
		_ => return Err(()),
	};
	if b > 0 {
		unimplemented!();
	}
	Ok(a * c)
}

#[derive(Clone, Debug)]
pub struct Envs {
	pub deploy: Option<Option<Deploy>>,
	pub version: Option<Option<bool>>,
	pub recce: Option<Option<bool>>,
	pub format: Option<Option<Format>>,
	pub resources: Option<Option<Resources>>,
}
impl Envs {
	pub fn from_env() -> Envs {
		let deploy = env::var_os("DEPLOY").map(|x| {
			x.into_string()
				.ok()
				.and_then(|x| match &*x.to_ascii_lowercase() {
					"fabric" => Some(Deploy::Fabric),
					_ => None,
				})
		}); // TODO: use serde?
		let version = env::var_os("DEPLOY_VERSION").map(|x| {
			x.into_string().ok().and_then(|x| match &*x {
				"0" => Some(false),
				"1" => Some(true),
				_ => None,
			})
		});
		let recce = env::var_os("DEPLOY_RECCE").map(|x| {
			x.into_string().ok().and_then(|x| match &*x {
				"0" => Some(false),
				"1" => Some(true),
				_ => None,
			})
		});
		let format = env::var_os("DEPLOY_FORMAT").map(|x| {
			x.into_string()
				.ok()
				.and_then(|x| match &*x.to_ascii_lowercase() {
					"human" => Some(Format::Human),
					"json" => Some(Format::Json),
					_ => None,
				})
		}); // TODO: use serde?
		let resources = env::var_os("DEPLOY_RESOURCES").map(|x| {
			x.into_string()
				.ok()
				.and_then(|x| serde_json::from_str(&x).ok())
		});
		Envs {
			deploy,
			version,
			recce,
			format,
			resources,
		}
	}

	pub fn from(env: &Vec<(ffi::OsString, ffi::OsString)>) -> Envs {
		let deploy = env.iter().find(|x| &x.0 == "DEPLOY").map(|x| {
			x.1
				.clone()
				.into_string()
				.ok()
				.and_then(|x| match &*x.to_ascii_lowercase() {
					"fabric" => Some(Deploy::Fabric),
					_ => None,
				})
		}); // TODO: use serde?
		let version = env.iter().find(|x| &x.0 == "DEPLOY_VERSION").map(|x| {
			x.1.clone().into_string().ok().and_then(|x| match &*x {
				"0" => Some(false),
				"1" => Some(true),
				_ => None,
			})
		});
		let recce = env.iter().find(|x| &x.0 == "DEPLOY_RECCE").map(|x| {
			x.1.clone().into_string().ok().and_then(|x| match &*x {
				"0" => Some(false),
				"1" => Some(true),
				_ => None,
			})
		});
		let format = env.iter().find(|x| &x.0 == "DEPLOY_FORMAT").map(|x| {
			x.1
				.clone()
				.into_string()
				.ok()
				.and_then(|x| match &*x.to_ascii_lowercase() {
					"human" => Some(Format::Human),
					"json" => Some(Format::Json),
					_ => None,
				})
		}); // TODO: use serde?
		let resources = env.iter().find(|x| &x.0 == "DEPLOY_RESOURCES").map(|x| {
			x.1
				.clone()
				.into_string()
				.ok()
				.and_then(|x| serde_json::from_str(&x).ok())
		});
		Envs {
			deploy,
			version,
			recce,
			format,
			resources,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Deploy {
	Fabric,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
	Human,
	Json,
}

/// Memory and CPU requirements for a process.
///
/// This is used in allocation of a process, to ensure that sufficient resources are available.
///
/// Best effort is made to enforce these as limits to avoid buggy/greedy processes starving others.
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Resources {
	/// Memory requirement in bytes
	pub mem: u64,
	/// CPU requirement as a fraction of one logical core. Any positive value is valid.
	pub cpu: f32,
}
impl Default for Resources {
	fn default() -> Resources {
		DEPLOY_RESOURCES_DEFAULT
	}
}
/// The [Resources] returned by [Resources::default()](Resources::default). Intended to be used as a placeholder in your application until you have a better idea as to resource requirements.
pub const DEPLOY_RESOURCES_DEFAULT: Resources = Resources {
	mem: 1024 * 1024 * 1024,
	cpu: 0.05,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(/*tag = "event", */rename_all = "lowercase")]
pub enum DeployOutputEvent {
	Spawn(Pid, Pid),
	Output(Pid, std::os::unix::io::RawFd, Vec<u8>),
	Exit(Pid, either::Either<u8, Signal>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DeployInputEvent {
	Input(Pid, std::os::unix::io::RawFd, Vec<u8>),
	Kill(Option<Pid>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum Signal {
	SIGHUP = nix::libc::SIGHUP,
	SIGINT = nix::libc::SIGINT,
	SIGQUIT = nix::libc::SIGQUIT,
	SIGILL = nix::libc::SIGILL,
	SIGTRAP = nix::libc::SIGTRAP,
	SIGABRT = nix::libc::SIGABRT,
	SIGBUS = nix::libc::SIGBUS,
	SIGFPE = nix::libc::SIGFPE,
	SIGKILL = nix::libc::SIGKILL,
	SIGUSR1 = nix::libc::SIGUSR1,
	SIGSEGV = nix::libc::SIGSEGV,
	SIGUSR2 = nix::libc::SIGUSR2,
	SIGPIPE = nix::libc::SIGPIPE,
	SIGALRM = nix::libc::SIGALRM,
	SIGTERM = nix::libc::SIGTERM,
	#[cfg(
		all(
			any(target_os = "linux", target_os = "android", target_os = "emscripten"),
			not(any(target_arch = "mips", target_arch = "mips64"))
		)
	)]
	SIGSTKFLT = nix::libc::SIGSTKFLT,
	SIGCHLD = nix::libc::SIGCHLD,
	SIGCONT = nix::libc::SIGCONT,
	SIGSTOP = nix::libc::SIGSTOP,
	SIGTSTP = nix::libc::SIGTSTP,
	SIGTTIN = nix::libc::SIGTTIN,
	SIGTTOU = nix::libc::SIGTTOU,
	SIGURG = nix::libc::SIGURG,
	SIGXCPU = nix::libc::SIGXCPU,
	SIGXFSZ = nix::libc::SIGXFSZ,
	SIGVTALRM = nix::libc::SIGVTALRM,
	SIGPROF = nix::libc::SIGPROF,
	SIGWINCH = nix::libc::SIGWINCH,
	SIGIO = nix::libc::SIGIO,
	#[cfg(any(target_os = "linux", target_os = "android", target_os = "emscripten"))]
	SIGPWR = nix::libc::SIGPWR,
	SIGSYS = nix::libc::SIGSYS,
	#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
	SIGEMT = nix::libc::SIGEMT,
	#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
	SIGINFO = nix::libc::SIGINFO,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProcessOutputEvent {
	Spawn(Pid),
	Output(os::unix::io::RawFd, Vec<u8>),
	Exit(either::Either<u8, Signal>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProcessInputEvent {
	Input(os::unix::io::RawFd, Vec<u8>),
	Kill,
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Clone, Copy, Debug)]
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
	pub fn color(&self, r: u8, g: u8, b: u8) -> Style {
		match &self.0 {
			&StyleSupport::None => *self,
			&StyleSupport::FourBit => unimplemented!(),
			&StyleSupport::EightBit => Style(
				self.0,
				self.1.fg(ansi_term::Colour::Fixed(
					16 + 36 * (r / 43) + 6 * (g / 43) + (b / 43),
				)),
			),
			&StyleSupport::TwentyFourBit => {
				Style(self.0, self.1.fg(ansi_term::Colour::RGB(r, g, b)))
			}
		}
	}

	pub fn bold(&self) -> Style {
		match &self.0 {
			&StyleSupport::None => *self,
			_ => Style(self.0, self.1.bold()),
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

fn pretty_pid<'a>(
	pid: &Pid, bold: bool, style_support: StyleSupport,
) -> ansi_term::ANSIGenericString<str> {
	// impl std::fmt::Display + 'a {
	let key: [u8; 16] = [0; 16];

	let bytes = encrypt(pid.0, key);
	let decrypted_data = decrypt(bytes, key);
	assert_eq!(&pid.0, &decrypted_data);

	let x = bytes.to_hex().take(7).collect::<String>();
	let mut rng = rand::XorShiftRng::from_seed([
		bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
		bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
	]);
	let (r, g, b) = loop {
		let (r_, g_, b_): (u8, u8, u8) = rng.gen();
		let (r, g, b) = (r_ as u16, g_ as u16, b_ as u16);
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

const STDOUT: os::unix::io::RawFd = 1;
const STDERR: os::unix::io::RawFd = 2;

struct Writer {
	// TODO: handle buffering better: write returns a referencing struct that buffers and flushes on drop
	fd: os::unix::io::RawFd,
	stdout: io::Stdout,
	stderr: io::Stderr,
}
impl Writer {
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
				file.into_inner().unwrap().into_raw_fd();
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
				file.into_raw_fd();
			}
		}
		self.fd = fd;
	}
}

pub struct Formatter {
	// TODO: what to do about encoding?
	writer: Writer,
	pid: Pid,
	nl: Option<os::unix::io::RawFd>,
	style_support: StyleSupport,
}
impl Formatter {
	pub fn new(pid: Pid, style_support: StyleSupport) -> Self {
		eprintln!("{}:", pretty_pid(&pid, true, style_support));
		Formatter {
			writer: Writer {
				fd: STDERR,
				stdout: io::stdout(),
				stderr: io::stderr(),
			},
			pid,
			nl: None,
			style_support,
		}
	}

	pub fn write(&mut self, event: &DeployOutputEvent) {
		match event {
			&DeployOutputEvent::Spawn(pid_, new_pid) => {
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
			&DeployOutputEvent::Output(pid_, fd, ref output) => {
				if output.len() > 0 {
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
							// assert!(output.len() > 0);
							if output.len() > 0 {
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
			&DeployOutputEvent::Exit(pid_, exit_code_) => {
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
						"   {} {:?}\n",
						self.style_support.style().bold().paint("exited:"),
						exit_code_
					),
				);
				// self.writer.write_fmt(STDERR, format_args!("   {} {:?}\nremaining: {}\n", self.style_support.style().bold().paint("exited:"), exit_code_, std::slice::SliceConcatExt::join(&*xyz.iter().map(|pid|pretty_pid(pid,false).to_string()).collect::<Vec<_>>(), ",")));
			}
		}
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

fn encrypt(input: [u8; 16], key: [u8; 16]) -> [u8; 16] {
	let mut output: [u8; 16] = unsafe { mem::uninitialized() };
	let mut round_keys: [u32; 44] = unsafe { mem::uninitialized() };
	aes_frast::aes_core::setkey_enc_k128(&key, &mut round_keys);
	aes_frast::aes_core::block_enc_k128(&input, &mut output, &round_keys);
	output
}
fn decrypt(input: [u8; 16], key: [u8; 16]) -> [u8; 16] {
	let mut output: [u8; 16] = unsafe { mem::uninitialized() };
	let mut round_keys: [u32; 44] = unsafe { mem::uninitialized() };
	aes_frast::aes_core::setkey_dec_k128(&key, &mut round_keys);
	aes_frast::aes_core::block_dec_k128(&input, &mut output, &round_keys);
	output
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn move_fds(fds: &mut [(std::os::unix::io::RawFd, std::os::unix::io::RawFd)]) {
	loop {
		let i = 'a: loop {
			for (i, &(from, to)) in fds.iter().enumerate() {
				if from == to {
					continue;
				}
				if fds.iter().position(|&(from, _)| from == to).is_none() {
					break 'a i;
				}
			}
			for &mut (from, to) in fds {
				assert_eq!(from, to);
			} // this assertion checks we aren't looping eternally due to a ring (todo: use F_DUPFD_CLOEXEC for temp fd)
			return;
		};
		let (from, to) = fds[i];
		let flags = nix::fcntl::FdFlag::from_bits(
			nix::fcntl::fcntl(from, nix::fcntl::FcntlArg::F_GETFD).unwrap(),
		).unwrap();
		let fd = nix::unistd::dup3(
			from,
			to,
			if flags.contains(nix::fcntl::FdFlag::FD_CLOEXEC) {
				nix::fcntl::OFlag::O_CLOEXEC
			} else {
				nix::fcntl::OFlag::empty()
			},
		).unwrap();
		assert_eq!(fd, to);
		nix::fcntl::fcntl(to, nix::fcntl::FcntlArg::F_SETFD(flags)).unwrap();
		nix::unistd::close(from).unwrap();
		fds[i].0 = to;
	}
}

pub fn seal(fd: std::os::unix::io::RawFd) {
	let fd2 = nix::fcntl::open(
		&path::PathBuf::from(format!("/proc/self/fd/{}", fd)),
		nix::fcntl::OFlag::O_RDONLY,
		nix::sys::stat::Mode::empty(),
	).unwrap();
	let flags = nix::fcntl::FdFlag::from_bits(
		nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).unwrap(),
	).unwrap();
	nix::unistd::close(fd).unwrap();
	nix::unistd::dup3(
		fd2,
		fd,
		if flags.contains(nix::fcntl::FdFlag::FD_CLOEXEC) {
			nix::fcntl::OFlag::O_CLOEXEC
		} else {
			nix::fcntl::OFlag::empty()
		},
	).unwrap();
	nix::unistd::close(fd2).unwrap();
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub struct FdIter(*mut nix::libc::DIR);
impl FdIter {
	pub fn new() -> Self {
		let dirp: *mut nix::libc::DIR =
			unsafe { nix::libc::opendir(b"/proc/self/fd\0".as_ptr() as *const i8) };
		assert!(dirp != ptr::null_mut());
		FdIter(dirp)
	}
}
impl Iterator for FdIter {
	// https://stackoverflow.com/questions/899038/getting-the-highest-allocated-file-descriptor/918469#918469
	type Item = os::unix::io::RawFd;

	fn next(&mut self) -> Option<Self::Item> {
		unsafe {
			let mut dent;
			while {
				dent = nix::libc::readdir(self.0);
				dent != ptr::null_mut()
			} {
				// https://github.com/rust-lang/rust/issues/34668
				let name = ffi::CStr::from_ptr((*dent).d_name.as_ptr());
				if &name == &ffi::CStr::from_ptr(b".\0".as_ptr() as *const i8)
					|| &name == &ffi::CStr::from_ptr(b"..\0".as_ptr() as *const i8)
				{
					continue;
				}
				let fd = name
					.to_str()
					.map_err(|_| ())
					.and_then(|fd| fd.parse::<os::unix::io::RawFd>().map_err(|_| ()));
				if fd.is_err() || fd.unwrap() == nix::libc::dirfd(self.0) {
					continue;
				}
				return Some(fd.unwrap());
			}
			return None;
		}
	}
}
impl ops::Drop for FdIter {
	fn drop(&mut self) {
		let ret = unsafe { nix::libc::closedir(self.0) };
		assert_eq!(ret, 0);
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn is_valgrind() -> bool {
	let _zzq_args: [u64; 6] = [4097, 0, 0, 0, 0, 0];
	let mut _zzq_result: u64 = 0;
	unsafe {
		asm!("rolq $$3,  %rdi; rolq $$13, %rdi; rolq $$61, %rdi; rolq $$51, %rdi; xchgq %rbx,%rbx;" : "+{rdx}" (_zzq_result) : "{rax}" (&_zzq_args[0]) : "cc", "memory" : "volatile")
	};
	_zzq_result > 0
}
pub fn valgrind_start_fd() -> os::unix::io::RawFd {
	let mut rlim: nix::libc::rlimit64 = unsafe { mem::uninitialized() };
	let err = unsafe { nix::libc::getrlimit64(nix::libc::RLIMIT_NOFILE, &mut rlim) };
	assert_eq!(err, 0);
	let valgrind_start_fd = rlim.rlim_max;
	assert!(
		valgrind_start_fd < i32::max_value() as u64,
		"{:?}",
		valgrind_start_fd
	);
	valgrind_start_fd as i32
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn map_bincode_err(err: bincode::Error) -> io::Error {
	match *err {
		bincode::ErrorKind::Io(err) => err,
		e => panic!("{:?}", e),
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn copy<R: ?Sized, W: ?Sized>(reader: &mut R, writer: &mut W, len: usize) -> io::Result<()>
where
	R: io::Read,
	W: io::Write,
{
	let mut offset = 0;
	while offset != len {
		offset += io::copy(&mut reader.take((len - offset) as u64), writer)? as usize;
	}
	Ok(())
}
pub fn copy_sendfile<O: AsRawFd, I: AsRawFd>(
	out: &O, in_: &I, len: usize,
) -> Result<(), nix::Error> {
	let mut offset = 0;
	while offset != len {
		offset +=
			nix::sys::sendfile::sendfile(out.as_raw_fd(), in_.as_raw_fd(), None, len - offset)?
	}
	Ok(())
}
pub fn copy_splice<O: AsRawFd, I: AsRawFd>(out: &O, in_: &I, len: usize) -> Result<(), nix::Error> {
	let mut offset = 0;
	while offset != len {
		offset += nix::fcntl::splice(
			in_.as_raw_fd(),
			None,
			out.as_raw_fd(),
			None,
			len - offset,
			nix::fcntl::SpliceFFlags::empty(),
		)?
	}
	Ok(())
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn memfd_create(
	name: &ffi::CStr, flags: nix::sys::memfd::MemFdCreateFlag,
) -> Result<std::os::unix::io::RawFd, nix::Error> {
	match nix::sys::memfd::memfd_create(name, flags) {
		Err(nix::Error::Sys(nix::errno::Errno::ENOSYS)) => {
			let mut random: [u8; 32] = unsafe { mem::uninitialized() };
			rand::thread_rng().fill(&mut random);
			let name = path::PathBuf::from(format!("/{}", random.to_hex()));
			nix::sys::mman::shm_open(
				&name,
				nix::fcntl::OFlag::O_RDWR | nix::fcntl::OFlag::O_CREAT | nix::fcntl::OFlag::O_EXCL,
				nix::sys::stat::Mode::S_IRWXU,
			).map(|fd| {
				if flags == nix::sys::memfd::MemFdCreateFlag::MFD_CLOEXEC {
				} else if flags == nix::sys::memfd::MemFdCreateFlag::empty() {
					let mut flags_ = nix::fcntl::FdFlag::from_bits(
						nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).unwrap(),
					).unwrap();
					flags_.remove(nix::fcntl::FdFlag::FD_CLOEXEC);
					nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(flags_)).unwrap();
				} else {
					unimplemented!()
				}
				nix::sys::mman::shm_unlink(&name).unwrap();
				fd
			})
		}
		a => a,
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub mod cargo_metadata {
	use cargo_metadata_;
	// pub use cargo_metadata_::*;
	use std::path::PathBuf;

	// https://github.com/rust-lang/cargo/blob/c24a09772c2c1cb315970dbc721f2a42d4515f21/src/cargo/util/machine_message.rs
	#[derive(Deserialize, Debug)]
	#[serde(tag = "reason", rename_all = "kebab-case")]
	pub enum Message {
		CompilerArtifact {
			#[serde(flatten)]
			artifact: Artifact,
		},
		CompilerMessage {},
		BuildScriptExecuted {},
		#[serde(skip)]
		Unknown, // TODO https://github.com/serde-rs/serde/issues/912
	}
	#[derive(Deserialize, Debug)]
	pub struct Artifact {
		pub package_id: String,
		pub target: cargo_metadata_::Target, // https://github.com/rust-lang/cargo/blob/c24a09772c2c1cb315970dbc721f2a42d4515f21/src/cargo/core/manifest.rs#L188
		pub profile: ArtifactProfile,
		pub features: Vec<String>,
		pub filenames: Vec<PathBuf>,
		pub fresh: bool,
	}
	#[derive(Deserialize, Debug)]
	pub struct ArtifactProfile {
		pub opt_level: String,
		pub debuginfo: Option<u32>,
		pub debug_assertions: bool,
		pub overflow_checks: bool,
		pub test: bool,
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

mod bufferedstream {
	use std::{
		io::{self, Write},
		ops,
	};
	pub struct BufferedStream<T: io::Read + io::Write> {
		stream: io::BufReader<T>,
	}
	impl<T: io::Read + io::Write> BufferedStream<T> {
		pub fn new(stream: T) -> Self {
			BufferedStream {
				stream: io::BufReader::new(stream),
			}
		}

		pub fn write<'a>(&'a mut self) -> BufferedStreamWriter<'a, T> {
			BufferedStreamWriter(io::BufWriter::new(self))
		}

		pub fn get_ref(&self) -> &T {
			self.stream.get_ref()
		}

		pub fn get_mut(&mut self) -> &mut T {
			self.stream.get_mut()
		}
	}
	impl<T: io::Read + io::Write> io::Read for BufferedStream<T> {
		fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
			self.stream.read(buf)
		}
	}
	impl<'a, T: io::Read + io::Write + 'a> io::Write for &'a mut BufferedStream<T> {
		fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
			self.stream.get_mut().write(buf)
		}

		fn flush(&mut self) -> io::Result<()> {
			self.stream.get_mut().flush()
		}
	}
	pub struct BufferedStreamWriter<'a, T: io::Read + io::Write + 'a>(
		io::BufWriter<&'a mut BufferedStream<T>>,
	);
	impl<'a, T: io::Read + io::Write + 'a> io::Write for BufferedStreamWriter<'a, T> {
		fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
			self.0.write(buf)
		}

		fn flush(&mut self) -> io::Result<()> {
			self.0.flush()
		}
	}
	impl<'a, T: io::Read + io::Write + 'a> ops::Drop for BufferedStreamWriter<'a, T> {
		fn drop(&mut self) {
			self.0.flush().unwrap();
		}
	}
}
pub use bufferedstream::BufferedStream;

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

mod hex {
	use std::{fmt, mem};
	#[derive(Clone)]
	pub struct Hex<'a>(&'a [u8], bool);
	impl<'a> Iterator for Hex<'a> {
		type Item = char;

		fn next(&mut self) -> Option<char> {
			if self.0.len() != 0 {
				const CHARS: &'static [u8] = b"0123456789abcdef";
				let byte = self.0[0];
				let second = self.1;
				if second {
					self.0 = self.0.split_first().unwrap().1;
				}
				self.1 = !self.1;
				Some(CHARS[if !second { byte >> 4 } else { byte & 0xf } as usize] as char)
			} else {
				None
			}
		}
	}
	impl<'a> fmt::Display for Hex<'a> {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			for char_ in self.clone() {
				write!(f, "{}", char_)?;
			}
			Ok(())
		}
	}
	pub trait ToHex {
		fn to_hex(&self) -> Hex; // TODO: make impl Iterator when poss
	}
	impl ToHex for [u8] {
		fn to_hex(&self) -> Hex {
			Hex(&*self, false)
		}
	}
	impl ToHex for [u16] {
		fn to_hex(&self) -> Hex {
			unsafe { mem::transmute::<&[u16], &[u8]>(self) }.to_hex()
		}
	}
	impl ToHex for [u32] {
		fn to_hex(&self) -> Hex {
			unsafe { mem::transmute::<&[u32], &[u8]>(self) }.to_hex()
		}
	}
	impl ToHex for [u64] {
		fn to_hex(&self) -> Hex {
			unsafe { mem::transmute::<&[u64], &[u8]>(self) }.to_hex()
		}
	}
}
pub use self::hex::ToHex;
