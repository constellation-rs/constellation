#![warn(
	// missing_copy_implementations,
	missing_debug_implementations,
	// missing_docs,
	trivial_numeric_casts,
	unused_extern_crates,
	unused_import_braces,
	unused_qualifications,
	unused_results,
	clippy::pedantic,
)] // from https://github.com/rust-unofficial/patterns/blob/master/anti_patterns/deny-warnings.md
#![allow(
	clippy::or_fun_call,
	clippy::trivially_copy_pass_by_ref,
	clippy::option_option,
	clippy::boxed_local,
	clippy::needless_pass_by_value,
	clippy::large_enum_variant,
	clippy::if_not_else,
	clippy::inline_always,
	clippy::all,
	warnings
)]

mod ext;
mod format;
pub mod msg;

#[cfg(unix)]
use nix::{fcntl, sys::signal, unistd};
use palaver::file::{copy, memfd_create};
use serde::{Deserialize, Serialize};
use std::{
	convert::TryInto, env, ffi::{CString, OsString}, fmt::{self, Debug, Display}, fs::File, io::{self, Read}, net, ops, os::unix::{
		ffi::OsStringExt, io::{AsRawFd, FromRawFd}
	}, sync::{Arc, Mutex}
};

#[cfg(target_family = "unix")]
pub type Fd = std::os::unix::io::RawFd;
#[cfg(target_family = "windows")]
pub type Fd = std::os::windows::io::RawHandle;

#[cfg(feature = "alloc_counter")]
#[global_allocator]
static A: alloc_counter::AllocCounterSystem = alloc_counter::AllocCounterSystem;

pub use ext::*;
pub use format::*;

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
	pub(crate) fn new(ip: net::IpAddr, port: u16) -> Self {
		match ip {
			net::IpAddr::V4(ip) => {
				let ip = ip.octets();
				Self([
					ip[0],
					ip[1],
					ip[2],
					ip[3],
					(port >> 8).try_into().unwrap(),
					(port & 0xff).try_into().unwrap(),
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
			((u16::from(self.0[4])) << 8) | (u16::from(self.0[5])),
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
impl Display for Pid {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}", self.format().take(7).collect::<String>())
	}
}
impl Debug for Pid {
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
	fn new(ip: net::IpAddr, port: u16) -> Self {
		Self::new(ip, port)
	}

	fn addr(&self) -> net::SocketAddr {
		Self::addr(self)
	}
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
	pub fn from_env() -> Self {
		let deploy = env::var_os("CONSTELLATION").map(|x| {
			x.into_string()
				.ok()
				.and_then(|x| match &*x.to_ascii_lowercase() {
					"fabric" => Some(Deploy::Fabric),
					_ => None,
				})
		}); // TODO: use serde?
		let version = env::var_os("CONSTELLATION_VERSION").map(|x| {
			x.into_string().ok().and_then(|x| match &*x {
				"0" => Some(false),
				"1" => Some(true),
				_ => None,
			})
		});
		let recce = env::var_os("CONSTELLATION_RECCE").map(|x| {
			x.into_string().ok().and_then(|x| match &*x {
				"0" => Some(false),
				"1" => Some(true),
				_ => None,
			})
		});
		let format = env::var_os("CONSTELLATION_FORMAT").map(|x| {
			x.into_string()
				.ok()
				.and_then(|x| match &*x.to_ascii_lowercase() {
					"human" => Some(Format::Human),
					"json" => Some(Format::Json),
					_ => None,
				})
		}); // TODO: use serde?
		let resources = env::var_os("CONSTELLATION_RESOURCES").map(|x| {
			x.into_string()
				.ok()
				.and_then(|x| serde_json::from_str(&x).ok())
		});
		Self {
			deploy,
			version,
			recce,
			format,
			resources,
		}
	}

	pub fn from(env: &[(OsString, OsString)]) -> Self {
		let deploy =
			env.iter().find_map(|x| {
				if x.0 == "CONSTELLATION" {
					Some(x.1.clone().into_string().ok().and_then(
						|x| match &*x.to_ascii_lowercase() {
							"fabric" => Some(Deploy::Fabric),
							_ => None,
						},
					))
				} else {
					None
				}
			}); // TODO: use serde?
		let version = env.iter().find_map(|x| {
			if x.0 == "CONSTELLATION_VERSION" {
				Some(x.1.clone().into_string().ok().and_then(|x| match &*x {
					"0" => Some(false),
					"1" => Some(true),
					_ => None,
				}))
			} else {
				None
			}
		});
		let recce = env.iter().find_map(|x| {
			if x.0 == "CONSTELLATION_RECCE" {
				Some(x.1.clone().into_string().ok().and_then(|x| match &*x {
					"0" => Some(false),
					"1" => Some(true),
					_ => None,
				}))
			} else {
				None
			}
		});
		let format =
			env.iter().find_map(|x| {
				if x.0 == "CONSTELLATION_FORMAT" {
					Some(x.1.clone().into_string().ok().and_then(
						|x| match &*x.to_ascii_lowercase() {
							"human" => Some(Format::Human),
							"json" => Some(Format::Json),
							_ => None,
						},
					))
				} else {
					None
				}
			}); // TODO: use serde?
		let resources = env.iter().find_map(|x| {
			if x.0 == "CONSTELLATION_RESOURCES" {
				Some(
					x.1.clone()
						.into_string()
						.ok()
						.and_then(|x| serde_json::from_str(&x).ok()),
				)
			} else {
				None
			}
		});
		Self {
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
	/// CPU requirement as a fraction of one logical core multiplied by 2^16.
	pub cpu: u32,
}
impl Default for Resources {
	fn default() -> Self {
		RESOURCES_DEFAULT
	}
}
/// The [Resources] returned by [`Resources::default()`](Resources::default). Intended to be used as a placeholder in your application until you have a better idea as to resource requirements.
pub const RESOURCES_DEFAULT: Resources = Resources {
	mem: 1024 * 1024 * 1024,
	cpu: 65536 / 16,
};

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(/*tag = "event", */rename_all = "lowercase")]
pub enum FabricOutputEvent {
	Init { pid: Pid, system_pid: u64 },
	Exit { pid: Pid, system_pid: u64 },
	// Spawn(Pid, Pid),
	// Output(Pid, Fd, Vec<u8>),
	// Exit(Pid, ExitStatus),
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(/*tag = "event", */rename_all = "lowercase")]
pub enum DeployOutputEvent {
	Spawn(Pid, Pid),
	Output(Pid, Fd, Vec<u8>),
	Exit(Pid, ExitStatus),
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum DeployInputEvent {
	Input(Pid, Fd, Vec<u8>),
	Kill(Option<Pid>),
}
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum ExitStatus {
	Success,
	Error(ExitStatusError),
}
impl ExitStatus {
	pub fn success(&self) -> bool {
		if let ExitStatus::Success = *self {
			true
		} else {
			false
		}
	}
	pub fn error(&self) -> Option<ExitStatusError> {
		if let ExitStatus::Error(error) = *self {
			Some(error)
		} else {
			None
		}
	}
	pub fn from_unix_status(s: u8) -> Self {
		if s == 0 {
			ExitStatus::Success
		} else {
			ExitStatus::Error(ExitStatusError::Unix(ExitStatusUnix::Status(s)))
		}
	}
	pub fn from_unix_signal(s: signal::Signal) -> Self {
		ExitStatus::Error(ExitStatusError::Unix(ExitStatusUnix::Signal(s.into())))
	}
}
impl ops::Add for ExitStatus {
	type Output = Self;
	fn add(self, other: Self) -> Self {
		match (self, other) {
			(a, b) if a == b => a,
			_ => ExitStatus::Error(ExitStatusError::Indeterminate),
		}
	}
}
impl ops::AddAssign for ExitStatus {
	fn add_assign(&mut self, other: Self) {
		*self = *self + other;
	}
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum ExitStatusError {
	Unix(ExitStatusUnix),
	Windows(u32), // https://msdn.microsoft.com/en-gb/library/cc231199.aspx
	Indeterminate,
}
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum ExitStatusUnix {
	Status(u8),
	Signal(Signal),
}

/// From [nix/src/sys/signal.rs](https://github.com/nix-rust/nix/blob/237ec7bc13d045f21ae653c74bfd41fe411860f9/src/sys/signal.rs#L23)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum Signal {
	SIGHUP,
	SIGINT,
	SIGQUIT,
	SIGILL,
	SIGTRAP,
	SIGABRT,
	SIGBUS,
	SIGFPE,
	SIGKILL,
	SIGUSR1,
	SIGSEGV,
	SIGUSR2,
	SIGPIPE,
	SIGALRM,
	SIGTERM,
	SIGSTKFLT,
	SIGCHLD,
	SIGCONT,
	SIGSTOP,
	SIGTSTP,
	SIGTTIN,
	SIGTTOU,
	SIGURG,
	SIGXCPU,
	SIGXFSZ,
	SIGVTALRM,
	SIGPROF,
	SIGWINCH,
	SIGIO,
	SIGPWR,
	SIGSYS,
	SIGEMT,
	SIGINFO,
}
impl From<signal::Signal> for Signal {
	fn from(signal: signal::Signal) -> Self {
		match signal {
			signal::Signal::SIGHUP => Signal::SIGHUP,
			signal::Signal::SIGINT => Signal::SIGINT,
			signal::Signal::SIGQUIT => Signal::SIGQUIT,
			signal::Signal::SIGILL => Signal::SIGILL,
			signal::Signal::SIGTRAP => Signal::SIGTRAP,
			signal::Signal::SIGABRT => Signal::SIGABRT,
			signal::Signal::SIGBUS => Signal::SIGBUS,
			signal::Signal::SIGFPE => Signal::SIGFPE,
			signal::Signal::SIGKILL => Signal::SIGKILL,
			signal::Signal::SIGUSR1 => Signal::SIGUSR1,
			signal::Signal::SIGSEGV => Signal::SIGSEGV,
			signal::Signal::SIGUSR2 => Signal::SIGUSR2,
			signal::Signal::SIGPIPE => Signal::SIGPIPE,
			signal::Signal::SIGALRM => Signal::SIGALRM,
			signal::Signal::SIGTERM => Signal::SIGTERM,
			#[cfg(all(
				any(target_os = "linux", target_os = "android", target_os = "emscripten"),
				not(any(target_arch = "mips", target_arch = "mips64"))
			))]
			signal::Signal::SIGSTKFLT => Signal::SIGSTKFLT,
			signal::Signal::SIGCHLD => Signal::SIGCHLD,
			signal::Signal::SIGCONT => Signal::SIGCONT,
			signal::Signal::SIGSTOP => Signal::SIGSTOP,
			signal::Signal::SIGTSTP => Signal::SIGTSTP,
			signal::Signal::SIGTTIN => Signal::SIGTTIN,
			signal::Signal::SIGTTOU => Signal::SIGTTOU,
			signal::Signal::SIGURG => Signal::SIGURG,
			signal::Signal::SIGXCPU => Signal::SIGXCPU,
			signal::Signal::SIGXFSZ => Signal::SIGXFSZ,
			signal::Signal::SIGVTALRM => Signal::SIGVTALRM,
			signal::Signal::SIGPROF => Signal::SIGPROF,
			signal::Signal::SIGWINCH => Signal::SIGWINCH,
			signal::Signal::SIGIO => Signal::SIGIO,
			#[cfg(any(target_os = "linux", target_os = "android", target_os = "emscripten"))]
			signal::Signal::SIGPWR => Signal::SIGPWR,
			signal::Signal::SIGSYS => Signal::SIGSYS,
			#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
			signal::Signal::SIGEMT => Signal::SIGEMT,
			#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
			signal::Signal::SIGINFO => Signal::SIGINFO,
		}
	}
}
impl From<Signal> for signal::Signal {
	fn from(signal: Signal) -> Self {
		match signal {
			Signal::SIGHUP => signal::Signal::SIGHUP,
			Signal::SIGINT => signal::Signal::SIGINT,
			Signal::SIGQUIT => signal::Signal::SIGQUIT,
			Signal::SIGILL => signal::Signal::SIGILL,
			Signal::SIGTRAP => signal::Signal::SIGTRAP,
			Signal::SIGABRT => signal::Signal::SIGABRT,
			Signal::SIGBUS => signal::Signal::SIGBUS,
			Signal::SIGFPE => signal::Signal::SIGFPE,
			Signal::SIGKILL => signal::Signal::SIGKILL,
			Signal::SIGUSR1 => signal::Signal::SIGUSR1,
			Signal::SIGSEGV => signal::Signal::SIGSEGV,
			Signal::SIGUSR2 => signal::Signal::SIGUSR2,
			Signal::SIGPIPE => signal::Signal::SIGPIPE,
			Signal::SIGALRM => signal::Signal::SIGALRM,
			Signal::SIGTERM => signal::Signal::SIGTERM,
			#[cfg(all(
				any(target_os = "linux", target_os = "android", target_os = "emscripten"),
				not(any(target_arch = "mips", target_arch = "mips64"))
			))]
			Signal::SIGSTKFLT => signal::Signal::SIGSTKFLT,
			Signal::SIGCHLD => signal::Signal::SIGCHLD,
			Signal::SIGCONT => signal::Signal::SIGCONT,
			Signal::SIGSTOP => signal::Signal::SIGSTOP,
			Signal::SIGTSTP => signal::Signal::SIGTSTP,
			Signal::SIGTTIN => signal::Signal::SIGTTIN,
			Signal::SIGTTOU => signal::Signal::SIGTTOU,
			Signal::SIGURG => signal::Signal::SIGURG,
			Signal::SIGXCPU => signal::Signal::SIGXCPU,
			Signal::SIGXFSZ => signal::Signal::SIGXFSZ,
			Signal::SIGVTALRM => signal::Signal::SIGVTALRM,
			Signal::SIGPROF => signal::Signal::SIGPROF,
			Signal::SIGWINCH => signal::Signal::SIGWINCH,
			Signal::SIGIO => signal::Signal::SIGIO,
			#[cfg(any(target_os = "linux", target_os = "android", target_os = "emscripten"))]
			Signal::SIGPWR => signal::Signal::SIGPWR,
			Signal::SIGSYS => signal::Signal::SIGSYS,
			#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
			Signal::SIGEMT => signal::Signal::SIGEMT,
			#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
			Signal::SIGINFO => signal::Signal::SIGINFO,
			_ => unimplemented!(),
		}
	}
}

impl From<ExitStatus> for i32 {
	fn from(exit_status: ExitStatus) -> Self {
		match exit_status {
			ExitStatus::Success => 0,
			ExitStatus::Error(error) => match error {
				ExitStatusError::Unix(ExitStatusUnix::Signal(signal)) => {
					signal::Signal::from(signal) as Self | (1 << 7)
				}
				ExitStatusError::Unix(ExitStatusUnix::Status(status)) => Self::from(status),
				ExitStatusError::Windows(code) => code.try_into().unwrap(),
				ExitStatusError::Indeterminate => 101,
			},
		}
	}
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum ProcessOutputEvent {
	Spawn(Pid),
	Output(Fd, Vec<u8>),
	Exit(ExitStatus),
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum ProcessInputEvent {
	Input(Fd, Vec<u8>),
	Kill,
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#[allow(missing_debug_implementations)]
#[derive(Clone)]
pub struct Trace<W: io::Write> {
	stdout: Arc<Mutex<W>>,
	format: Format,
	verbose: bool,
}
impl<W: io::Write> Trace<W> {
	pub fn new(stdout: W, format: Format, verbose: bool) -> Self {
		Self {
			stdout: Arc::new(Mutex::new(stdout)),
			format,
			verbose,
		}
	}
	fn json<T: Serialize>(&self, _event: T) {
		// let mut stdout = self.stdout.lock().unwrap();
		// serde_json::to_writer(&mut *stdout, &event).unwrap();
		// stdout.write_all(b"\n").unwrap()
	}
	fn human<T: Debug>(&self, _event: T) {
		// // TODO: Display
		// let mut stdout = self.stdout.lock().unwrap();
		// stdout.write_fmt(format_args!("{:?}", event)).unwrap()
	}
	pub fn fabric(&self, event: FabricOutputEvent) {
		match (self.format, self.verbose) {
			(Format::Json, true) => self.json(event),
			(Format::Human, true) => self.human(event),
			_ => (),
		}
	}
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn file_from_reader<R: Read>(
	reader: &mut R, len: u64, name: &OsString, cloexec: bool,
) -> Result<File, io::Error> {
	let mut file = unsafe {
		File::from_raw_fd(
			memfd_create(
				&CString::new(OsStringExt::into_vec(name.clone())).unwrap(),
				cloexec,
			)
			.expect("Failed to memfd_create"),
		)
	};
	assert_eq!(
		fcntl::FdFlag::from_bits(fcntl::fcntl(file.as_raw_fd(), fcntl::FcntlArg::F_GETFD).unwrap())
			.unwrap()
			.contains(fcntl::FdFlag::FD_CLOEXEC),
		cloexec
	);
	unistd::ftruncate(file.as_raw_fd(), len.try_into().unwrap()).unwrap();
	copy(reader, &mut file, len)?;
	let x = unistd::lseek(file.as_raw_fd(), 0, unistd::Whence::SeekSet).unwrap();
	assert_eq!(x, 0);
	Ok(file)
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn map_bincode_err(err: bincode::Error) -> io::Error {
	match *err {
		bincode::ErrorKind::Io(err) => err,
		e => panic!("{:?}", e),
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn forbid_alloc<F, R>(f: F) -> R
where
	F: FnOnce() -> R,
{
	#[cfg(feature = "alloc_counter")]
	{
		alloc_counter::forbid_alloc(f)
	}
	#[cfg(not(feature = "alloc_counter"))]
	{
		f()
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub mod cargo_metadata {
	use cargo_metadata::Target;
	use serde::Deserialize;
	use std::path::PathBuf;

	// https://github.com/rust-lang/cargo/blob/c24a09772c2c1cb315970dbc721f2a42d4515f21/src/cargo/util/machine_message.rs
	#[derive(Deserialize, Debug)]
	#[serde(tag = "reason", rename_all = "kebab-case")]
	#[allow(clippy::pub_enum_variant_names)]
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
		pub target: Target, // https://github.com/rust-lang/cargo/blob/c24a09772c2c1cb315970dbc721f2a42d4515f21/src/cargo/core/manifest.rs#L188
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
