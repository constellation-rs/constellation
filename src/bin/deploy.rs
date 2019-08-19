//! # `deploy`
//! Run a binary on a constellation cluster
//!
//! ## Usage
//! ```text
//! deploy [options] <host> <binary> [--] [args]...
//! ```
//!
//! ## Options
//! ```text
//! -h --help          Show this screen.
//! -V --version       Show version.
//! --format=<fmt>     Output format [possible values: human, json] [defa ult: human]
//! ```
//!
//! Note: --format can also be given as an env var, such as `CONSTELLATION_FORMAT=json`

#![warn(
	missing_copy_implementations,
	missing_debug_implementations,
	missing_docs,
	trivial_numeric_casts,
	unused_extern_crates,
	unused_import_braces,
	unused_qualifications,
	unused_results,
	clippy::pedantic
)] // from https://github.com/rust-unofficial/patterns/blob/master/anti_patterns/deny-warnings.md

use either::Either;
use serde::Deserialize;
use std::{
	collections::HashSet, env, ffi, fs, io::{self, Read, Write}, iter, mem, net, path, process
};

use constellation_internal::{
	map_bincode_err, msg::{bincode_serialize_into, BridgeRequest}, BufferedStream, DeployInputEvent, DeployOutputEvent, Envs, ExitStatus, Format, Formatter, Pid, StyleSupport
};

const USAGE: &str = "Run a binary on a constellation cluster.

USAGE:
    deploy [options] <host> <binary> [--] [args]...

OPTIONS:
    -h --help          Show this screen.
    -V --version       Show version.
    --format=<fmt>     Output format [possible values: human, json] [defa ult: human]

Note: --format can also be given as an env var, such as CONSTELLATION_FORMAT=json
";

#[derive(Debug, Deserialize)]
struct Args {
	flag_version: bool,
	flag_format: Option<Format>,
	arg_host: String,
	arg_binary: path::PathBuf,
	arg_args: Vec<String>, // ffi::OsString
}

fn main() {
	let envs = Envs::from_env();
	let args: Args = docopt::Docopt::new(USAGE)
		.and_then(|d| d.deserialize())
		.unwrap_or_else(|e| e.exit());
	let version = args.flag_version
		|| envs
			.version
			.map_or(false, |x| x.expect("CONSTELLATION_VERSION must be 0 or 1"));
	let format = args
		.flag_format
		.or_else(|| {
			envs.format
				.map(|x| x.expect("CONSTELLATION_FORMAT must be json or human"))
		})
		.unwrap_or(Format::Human);
	if version {
		println!("constellation-deploy {}", env!("CARGO_PKG_VERSION"));
		process::exit(0);
	}
	let bridge_address: net::SocketAddr = args.arg_host.parse().unwrap();
	let path = args.arg_binary;
	let args: Vec<ffi::OsString> = iter::once(ffi::OsString::from(path.clone()))
		.chain(args.arg_args.into_iter().map(ffi::OsString::from))
		.collect();
	let vars: Vec<(ffi::OsString, ffi::OsString)> = env::vars_os().collect();
	let stream = net::TcpStream::connect(&bridge_address)
		.unwrap_or_else(|e| panic!("Could not connect to {:?}: {:?}", bridge_address, e));
	let (mut stream_read, mut stream_write) =
		(BufferedStream::new(&stream), BufferedStream::new(&stream));
	#[cfg(feature = "distribute_binaries")]
	let binary =
		fs::File::open(&path).unwrap_or_else(|e| panic!("Couldn't open file {:?}: {:?}", path, e));
	#[cfg(not(feature = "distribute_binaries"))]
	let binary = std::marker::PhantomData::<fs::File>;
	let request = BridgeRequest {
		resources: None,
		args,
		vars,
		arg: vec![],
		binary,
	};
	bincode_serialize_into(&mut stream_write.write(), &request)
		.map_err(map_bincode_err)
		.unwrap_or_else(|e| panic!("Couldn't communicate with bridge: {:?}", e));
	let pid: Option<Pid> = bincode::deserialize_from(&mut stream_read)
		.map_err(map_bincode_err)
		.unwrap_or_else(|e| panic!("Couldn't communicate with bridge: {:?}", e));
	let pid = pid.unwrap_or_else(|| {
		panic!("Deploy failed due to not being able to allocate process to any of the nodes or constellation::init() not being called immediately inside main()")
	}); // TODO get resources from bridge
	crossbeam::scope(|scope| {
		let _ = scope.spawn(|_scope| {
			let mut stdin = io::stdin();
			loop {
				let mut buf: [u8; 1024] = unsafe { mem::uninitialized() };
				let n = stdin.read(&mut buf).unwrap();
				bincode::serialize_into(
					&mut stream_write.write(),
					&DeployInputEvent::Input(pid, 0, buf[..n].to_owned()),
				)
				.unwrap();
				if n == 0 {
					break;
				}
			}
		});
		let mut exit_code = ExitStatus::Success;
		let mut ref_count = 1;
		let mut pids = HashSet::new();
		let _ = pids.insert(pid);
		let (stdout, stderr) = (io::stdout(), io::stderr());
		let mut formatter = if let Format::Human = format {
			Either::Left(Formatter::new(
				pid,
				if atty::is(atty::Stream::Stderr) {
					StyleSupport::EightBit
				} else {
					StyleSupport::None
				},
				stdout.lock(),
				stderr.lock(),
			))
		} else {
			Either::Right(stdout.lock())
		};
		loop {
			let event: DeployOutputEvent = bincode::deserialize_from(&mut stream_read)
				.map_err(map_bincode_err)
				.expect("Bridge died");
			match formatter {
				Either::Left(ref mut formatter) => formatter.write(&event),
				Either::Right(ref mut stdout) => {
					serde_json::to_writer(&mut *stdout, &event).unwrap();
					stdout.write_all(b"\n").unwrap()
				}
			}
			match event {
				DeployOutputEvent::Spawn(pid, new_pid) => {
					assert_ne!(pid, new_pid);
					assert!(pids.contains(&pid));
					ref_count += 1;
					let x = pids.insert(new_pid);
					assert!(x);
				}
				DeployOutputEvent::Output(pid, _fd, _output) => {
					assert!(pids.contains(&pid));
				}
				DeployOutputEvent::Exit(pid, exit_code_) => {
					exit_code += exit_code_;
					ref_count -= 1;
					let x = pids.remove(&pid);
					assert!(x);
					// printer.eprint(format_args!("   {} {:?}\nremaining: {}\n", ansi_term::Style::new().bold().paint("exited:"), exit_code_, std::slice::SliceConcatExt::join(&*pids.iter().map(|pid|pretty_pid(pid,false).to_string()).collect::<Vec<_>>(), ",")));
				}
			}
			if ref_count == 0 {
				break;
			}
		}
		process::exit(exit_code.into());
	})
	.unwrap();
}
