//! # `deploy`
//! Run a binary on a fabric cluster
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
//! Note: --format can also be given as an env var, such as DEPLOY_FORMAT=json

#![feature(nll, allocator_api, global_allocator)]
#![deny(missing_docs, deprecated)]

extern crate bincode;
extern crate crossbeam;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate serde_derive;
extern crate atty;
extern crate deploy_common;
extern crate docopt;
extern crate either;

use deploy_common::{
	copy_sendfile, map_bincode_err, BufferedStream, DeployInputEvent, DeployOutputEvent, Envs, Format, Formatter, Pid, Resources, StyleSupport
};
use either::Either;
use std::{
	collections::HashSet, env, ffi, fs, io::{self, Read, Write}, iter, mem, net, path, process
};

#[global_allocator]
static GLOBAL: std::alloc::System = std::alloc::System;

const USAGE: &'static str = "
deploy
Run a binary on a fabric cluster

USAGE:
    deploy [options] <host> <binary> [--] [args]...

OPTIONS:
    -h --help          Show this screen.
    -V --version       Show version.
    --format=<fmt>     Output format [possible values: human, json] [defa ult: human]

Note: --format can also be given as an env var, such as DEPLOY_FORMAT=json
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
	let _version = args.flag_version || envs
		.version
		.map(|x| x.expect("DEPLOY_VERSION must be 0 or 1"))
		.unwrap_or(false);
	let format = args
		.flag_format
		.or_else(|| {
			envs.format
				.map(|x| x.expect("DEPLOY_FORMAT must be json or human"))
		}).unwrap_or(Format::Human);
	let bridge_address: net::SocketAddr = args.arg_host.parse().unwrap();
	let path = args.arg_binary;
	let args: Vec<ffi::OsString> = iter::once(ffi::OsString::from(path.clone()))
		.chain(args.arg_args.into_iter().map(|x| ffi::OsString::from(x)))
		.collect();
	let vars: Vec<(ffi::OsString, ffi::OsString)> = env::vars_os().collect();
	let stream = net::TcpStream::connect(&bridge_address)
		.unwrap_or_else(|e| panic!("Could not connect to {:?}: {:?}", bridge_address, e));
	let (mut stream_read, mut stream_write) =
		(BufferedStream::new(&stream), BufferedStream::new(&stream));
	let elf = fs::File::open(path).unwrap();
	let len: u64 = elf.metadata().unwrap().len();
	assert_ne!(len, 0);
	let mut stream_write_ = stream_write.write();
	bincode::serialize_into(&mut stream_write_, &None::<Resources>).unwrap();
	bincode::serialize_into(&mut stream_write_, &args).unwrap();
	bincode::serialize_into(&mut stream_write_, &vars).unwrap();
	bincode::serialize_into(&mut stream_write_, &len).unwrap();
	mem::drop(stream_write_);
	copy_sendfile(&**stream_write.get_ref(), &elf, len as usize).unwrap();
	let mut stream_write_ = stream_write.write();
	let arg: Vec<u8> = Vec::new();
	bincode::serialize_into(&mut stream_write_, &arg).unwrap();
	mem::drop(stream_write_);
	let pid: Option<Pid> = bincode::deserialize_from(&mut stream_read)
		.map_err(map_bincode_err)
		.unwrap();
	let pid = pid.unwrap_or_else(|| {
		panic!("Deploy failed due to not being able to allocate process to any of the nodes")
	}); // TODO get resources from bridge
	crossbeam::scope(|scope| {
		scope.spawn(|| {
			let mut stdin = io::stdin();
			loop {
				let mut buf: [u8; 1024] = unsafe { mem::uninitialized() };
				let n = stdin.read(&mut buf).unwrap();
				bincode::serialize_into(
					&mut stream_write.write(),
					&DeployInputEvent::Input(pid, 0, buf[..n].to_owned()),
				).unwrap();
				if n == 0 {
					break;
				}
			}
		});
		let mut exit_code = None;
		let mut ref_count = 1;
		let mut xyz = HashSet::new();
		xyz.insert(pid);
		let mut formatter = if let Format::Human = format {
			Either::Left(Formatter::new(
				pid,
				if atty::is(atty::Stream::Stderr) {
					StyleSupport::EightBit
				} else {
					StyleSupport::None
				},
			))
		} else {
			Either::Right(io::stdout())
		};
		loop {
			let event: DeployOutputEvent = bincode::deserialize_from(&mut stream_read)
				.map_err(map_bincode_err)
				.expect("Bridge died");
			match &mut formatter {
				&mut Either::Left(ref mut formatter) => formatter.write(&event),
				&mut Either::Right(ref mut stdout) => {
					serde_json::to_writer(&mut *stdout, &event).unwrap();
					stdout.write_all(b"\n").unwrap()
				}
			}
			match event {
				DeployOutputEvent::Spawn(pid, new_pid) => {
					assert_ne!(pid, new_pid);
					assert!(xyz.contains(&pid));
					ref_count += 1;
					let x = xyz.insert(new_pid);
					assert!(x);
				}
				DeployOutputEvent::Output(pid, _fd, _output) => {
					assert!(xyz.contains(&pid));
				}
				DeployOutputEvent::Exit(pid, exit_code_) => {
					if exit_code_ != either::Either::Left(0) {
						if exit_code.is_none() {
							exit_code = Some(exit_code_); // TODO: nondeterministic
						}
					}
					ref_count -= 1;
					let x = xyz.remove(&pid);
					assert!(x);
					// printer.eprint(format_args!("   {} {:?}\nremaining: {}\n", ansi_term::Style::new().bold().paint("exited:"), exit_code_, std::slice::SliceConcatExt::join(&*xyz.iter().map(|pid|pretty_pid(pid,false).to_string()).collect::<Vec<_>>(), ",")));
				}
			}
			if ref_count == 0 {
				break;
			}
		}
		process::exit(
			exit_code
				.unwrap_or(either::Either::Left(0))
				.left()
				.unwrap_or(1) as i32,
		);
	});
}
