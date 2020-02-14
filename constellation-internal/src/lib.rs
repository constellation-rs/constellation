#![doc(html_root_url = "https://docs.rs/constellation-internal/0.2.0-alpha.1")]
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
	clippy::must_use_candidate,
	clippy::double_must_use,
	clippy::missing_errors_doc
)]

mod ext;
mod format;
pub mod msg;
mod units;

#[cfg(unix)]
use nix::{fcntl, libc, sys::signal, unistd};
use palaver::file::{copy, memfd_create};
use serde::{Deserialize, Serialize};
use std::{
	convert::{TryFrom, TryInto}, env, error::Error, ffi::{CString, OsString}, fmt::{self, Debug, Display}, fs::File, io::{self, Read, Seek, Write}, net::{IpAddr, SocketAddr}, ops, os::unix::{
		ffi::OsStringExt, io::{AsRawFd, FromRawFd, IntoRawFd}
	}, process::abort, sync::{Arc, Mutex}
};

#[cfg(target_family = "unix")]
pub type Fd = std::os::unix::io::RawFd;
#[cfg(target_family = "windows")]
pub type Fd = std::os::windows::io::RawHandle;

#[cfg(feature = "no_alloc")]
#[global_allocator]
static A: alloc_counter::AllocCounterSystem = alloc_counter::AllocCounterSystem;

pub use ext::*;
pub use format::*;
pub use units::*;

/// A process identifier.
///
/// The current process's `Pid` can be retrieved with [pid()](pid).
///
/// Unlike typical OS pids, it is:
///  * Universally unique – that is to say, the same `Pid` will never be seen twice;
///  * When running across a cluster, it is valid and unique cluster-wide, rather than within a single node.
///
/// All inter-process communication occurs after [Sender](Sender)s and [Receiver](Receiver)s have been created with their remotes' `Pid`s. Thus `Pid`s are the primary form of addressing in a `constellation` cluster.
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Pid {
	key: u128,
	ip: IpAddr,
	port: u16,
}
impl Pid {
	pub(crate) fn new(ip: IpAddr, port: u16) -> Self {
		assert_ne!(port, 0);
		let key = rand::random();
		Self { key, ip, port }
	}

	pub(crate) fn addr(&self) -> SocketAddr {
		SocketAddr::new(self.ip, self.port)
	}

	fn format<'a>(&'a self) -> impl Iterator<Item = char> + Clone + 'a {
		self.key
			.to_le_bytes()
			.to_hex()
			.take(7)
			.collect::<Vec<_>>()
			.into_iter()
	}
}
impl Display for Pid {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}", self.format().collect::<String>())
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
	fn new(ip: IpAddr, port: u16) -> Pid;
	fn addr(&self) -> SocketAddr;
}
#[doc(hidden)]
impl PidInternal for Pid {
	fn new(ip: IpAddr, port: u16) -> Self {
		Self::new(ip, port)
	}

	fn addr(&self) -> SocketAddr {
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
///
/// The default is [`RESOURCES_DEFAULT`], which is defined as:
///
/// ```ignore
/// # use constellation_internal::{Cpu, Mem, Resources};
/// pub const RESOURCES_DEFAULT: Resources = Resources {
///     mem: 100 * Mem::MIB, // 100 MiB
///     cpu: Cpu::CORE / 16, // 1/16th of a logical CPU core
/// };
/// ```
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Resources {
	/// Memory requirement in bytes
	pub mem: Mem,
	/// CPU requirement as a fraction of one logical core
	pub cpu: Cpu,
}
impl Default for Resources {
	fn default() -> Self {
		RESOURCES_DEFAULT
	}
}
/// The [Resources] returned by [`Resources::default()`](Resources::default). Intended to be used as a placeholder in your application until you have a better idea as to resource requirements.
///
/// ```ignore
/// # use constellation_internal::{Cpu, Mem, Resources};
/// pub const RESOURCES_DEFAULT: Resources = Resources {
///     mem: 100 * Mem::MIB, // 100 MiB
///     cpu: Cpu::CORE / 16, // 1/16th of a logical CPU core
/// };
/// ```
pub const RESOURCES_DEFAULT: Resources = Resources {
	mem: Mem(100 * 1024 * 2014), // 100 MiB
	cpu: Cpu(65536 / 16),        // 1/16th of a logical CPU core
};

/// An error returned by the [`try_spawn()`](try_spawn) method detailing the reason if known.
#[allow(missing_copy_implementations)]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrySpawnError {
	/// [`try_spawn()`](try_spawn) failed because the new process couldn't be allocated.
	NoCapacity,
	/// [`try_spawn()`](try_spawn) failed because `constellation::init()` is not called immediately inside main().
	Recce,
	/// [`try_spawn()`](try_spawn) failed for unknown reasons.
	Unknown,
	#[doc(hidden)]
	__Nonexhaustive, // https://github.com/rust-lang/rust/issues/44109
}

/// An error returned by the [`spawn()`](spawn) method detailing the reason if known.
#[allow(missing_copy_implementations)]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpawnError {
	/// [`spawn()`](spawn) failed because `constellation::init()` is not called immediately inside main().
	Recce,
	/// [`spawn()`](spawn) failed for unknown reasons.
	Unknown,
	#[doc(hidden)]
	__Nonexhaustive,
}
impl From<SpawnError> for TrySpawnError {
	fn from(error: SpawnError) -> Self {
		match error {
			SpawnError::Recce => Self::Recce,
			SpawnError::Unknown => Self::Unknown,
			SpawnError::__Nonexhaustive => unreachable!(),
		}
	}
}
impl TryFrom<TrySpawnError> for SpawnError {
	type Error = ();

	fn try_from(error: TrySpawnError) -> Result<Self, Self::Error> {
		match error {
			TrySpawnError::NoCapacity => Err(()),
			TrySpawnError::Recce => Ok(Self::Recce),
			TrySpawnError::Unknown => Ok(Self::Unknown),
			TrySpawnError::__Nonexhaustive => unreachable!(),
		}
	}
}
impl Display for TrySpawnError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::NoCapacity => write!(
				f,
				"try_spawn() failed because the new process couldn't be allocated"
			),
			Self::Recce => write!(
				f,
				"try_spawn() because constellation::init() is not called immediately inside main()"
			),
			Self::Unknown => write!(f, "try_spawn() failed for unknown reasons"),
			Self::__Nonexhaustive => unreachable!(),
		}
	}
}
impl Debug for TrySpawnError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		Display::fmt(self, f)
	}
}
impl Display for SpawnError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Recce => write!(
				f,
				"spawn() because constellation::init() is not called immediately inside main()"
			),
			Self::Unknown => write!(f, "spawn() failed for unknown reasons"),
			Self::__Nonexhaustive => unreachable!(),
		}
	}
}
impl Debug for SpawnError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		Display::fmt(self, f)
	}
}
impl Error for TrySpawnError {}
impl Error for SpawnError {}

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
		if let Self::Success = *self {
			true
		} else {
			false
		}
	}
	pub fn error(&self) -> Option<ExitStatusError> {
		if let Self::Error(error) = *self {
			Some(error)
		} else {
			None
		}
	}
	pub fn from_unix_status(s: u8) -> Self {
		if s == 0 {
			Self::Success
		} else {
			Self::Error(ExitStatusError::Unix(ExitStatusUnix::Status(s)))
		}
	}
	pub fn from_unix_signal(s: signal::Signal) -> Self {
		Self::Error(ExitStatusError::Unix(ExitStatusUnix::Signal(s.into())))
	}
}
impl ops::Add for ExitStatus {
	type Output = Self;
	fn add(self, other: Self) -> Self {
		match (self, other) {
			(a, b) if a == b => a,
			_ => Self::Error(ExitStatusError::Indeterminate),
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
			signal::Signal::SIGHUP => Self::SIGHUP,
			signal::Signal::SIGINT => Self::SIGINT,
			signal::Signal::SIGQUIT => Self::SIGQUIT,
			signal::Signal::SIGILL => Self::SIGILL,
			signal::Signal::SIGTRAP => Self::SIGTRAP,
			signal::Signal::SIGABRT => Self::SIGABRT,
			signal::Signal::SIGBUS => Self::SIGBUS,
			signal::Signal::SIGFPE => Self::SIGFPE,
			signal::Signal::SIGKILL => Self::SIGKILL,
			signal::Signal::SIGUSR1 => Self::SIGUSR1,
			signal::Signal::SIGSEGV => Self::SIGSEGV,
			signal::Signal::SIGUSR2 => Self::SIGUSR2,
			signal::Signal::SIGPIPE => Self::SIGPIPE,
			signal::Signal::SIGALRM => Self::SIGALRM,
			signal::Signal::SIGTERM => Self::SIGTERM,
			#[cfg(all(
				any(target_os = "linux", target_os = "android", target_os = "emscripten"),
				not(any(target_arch = "mips", target_arch = "mips64"))
			))]
			signal::Signal::SIGSTKFLT => Self::SIGSTKFLT,
			signal::Signal::SIGCHLD => Self::SIGCHLD,
			signal::Signal::SIGCONT => Self::SIGCONT,
			signal::Signal::SIGSTOP => Self::SIGSTOP,
			signal::Signal::SIGTSTP => Self::SIGTSTP,
			signal::Signal::SIGTTIN => Self::SIGTTIN,
			signal::Signal::SIGTTOU => Self::SIGTTOU,
			signal::Signal::SIGURG => Self::SIGURG,
			signal::Signal::SIGXCPU => Self::SIGXCPU,
			signal::Signal::SIGXFSZ => Self::SIGXFSZ,
			signal::Signal::SIGVTALRM => Self::SIGVTALRM,
			signal::Signal::SIGPROF => Self::SIGPROF,
			signal::Signal::SIGWINCH => Self::SIGWINCH,
			signal::Signal::SIGIO => Self::SIGIO,
			#[cfg(any(target_os = "linux", target_os = "android", target_os = "emscripten"))]
			signal::Signal::SIGPWR => Self::SIGPWR,
			signal::Signal::SIGSYS => Self::SIGSYS,
			#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
			signal::Signal::SIGEMT => Self::SIGEMT,
			#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
			signal::Signal::SIGINFO => Self::SIGINFO,
		}
	}
}
impl From<Signal> for signal::Signal {
	fn from(signal: Signal) -> Self {
		match signal {
			Signal::SIGHUP => Self::SIGHUP,
			Signal::SIGINT => Self::SIGINT,
			Signal::SIGQUIT => Self::SIGQUIT,
			Signal::SIGILL => Self::SIGILL,
			Signal::SIGTRAP => Self::SIGTRAP,
			Signal::SIGABRT => Self::SIGABRT,
			Signal::SIGBUS => Self::SIGBUS,
			Signal::SIGFPE => Self::SIGFPE,
			Signal::SIGKILL => Self::SIGKILL,
			Signal::SIGUSR1 => Self::SIGUSR1,
			Signal::SIGSEGV => Self::SIGSEGV,
			Signal::SIGUSR2 => Self::SIGUSR2,
			Signal::SIGPIPE => Self::SIGPIPE,
			Signal::SIGALRM => Self::SIGALRM,
			Signal::SIGTERM => Self::SIGTERM,
			#[cfg(all(
				any(target_os = "linux", target_os = "android", target_os = "emscripten"),
				not(any(target_arch = "mips", target_arch = "mips64"))
			))]
			Signal::SIGSTKFLT => Self::SIGSTKFLT,
			Signal::SIGCHLD => Self::SIGCHLD,
			Signal::SIGCONT => Self::SIGCONT,
			Signal::SIGSTOP => Self::SIGSTOP,
			Signal::SIGTSTP => Self::SIGTSTP,
			Signal::SIGTTIN => Self::SIGTTIN,
			Signal::SIGTTOU => Self::SIGTTOU,
			Signal::SIGURG => Self::SIGURG,
			Signal::SIGXCPU => Self::SIGXCPU,
			Signal::SIGXFSZ => Self::SIGXFSZ,
			Signal::SIGVTALRM => Self::SIGVTALRM,
			Signal::SIGPROF => Self::SIGPROF,
			Signal::SIGWINCH => Self::SIGWINCH,
			Signal::SIGIO => Self::SIGIO,
			#[cfg(any(target_os = "linux", target_os = "android", target_os = "emscripten"))]
			Signal::SIGPWR => Self::SIGPWR,
			Signal::SIGSYS => Self::SIGSYS,
			#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
			Signal::SIGEMT => Self::SIGEMT,
			#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
			Signal::SIGINFO => Self::SIGINFO,
			_ => unimplemented!(),
		}
	}
}

#[allow(clippy::use_self)] // TODO: remove; bug in clippy
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
pub struct Trace<W: Write> {
	stdout: Arc<Mutex<W>>,
	format: Format,
	verbose: bool,
}
impl<W: Write> Trace<W> {
	pub fn new(stdout: W, format: Format, verbose: bool) -> Self {
		Self {
			stdout: Arc::new(Mutex::new(stdout)),
			format,
			verbose,
		}
	}
	fn json<T: Serialize>(&self, event: T) {
		let mut stdout = self.stdout.lock().unwrap();
		serde_json::to_writer(&mut *stdout, &event).unwrap();
		stdout.write_all(b"\n").unwrap()
	}
	fn human<T: Debug>(&self, event: T) {
		// TODO: Display
		let mut stdout = self.stdout.lock().unwrap();
		stdout.write_fmt(format_args!("{:?}", event)).unwrap()
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
	let x = file.seek(io::SeekFrom::Start(0)).unwrap();
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
	#[cfg(feature = "no_alloc")]
	{
		alloc_counter::forbid_alloc(f)
	}
	#[cfg(not(feature = "no_alloc"))]
	{
		f()
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

struct StdErr;
impl Write for StdErr {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		let mut file = unsafe { File::from_raw_fd(libc::STDERR_FILENO) };
		let ret = file.write(buf);
		let _ = file.into_raw_fd();
		ret
	}
	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}

#[inline]
fn abort_on_unwind_<F: FnOnce() -> T, T>(f: F) -> T {
	replace_with::on_unwind(f, || {
		let _ = StdErr.write_all(b"Constellation: detected unexpected panic; aborting\n");
		abort();
	})
}

#[must_use]
#[inline]
pub fn abort_on_unwind<F: FnOnce() -> T, T>(f: F) -> impl FnOnce() -> T {
	|| abort_on_unwind_(f)
}

#[must_use]
#[inline]
pub fn abort_on_unwind_1<F: FnOnce(&A) -> T, T, A>(f: F) -> impl FnOnce(&A) -> T {
	|a| abort_on_unwind_(|| f(a))
}
