//! # `deploy`
//! Run a binary on a constellation cluster
//!
//! ## Usage
//! ```text
//! deploy [options] <host> <binary> [--] [<args>]...
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

#![cfg_attr(feature = "nightly", feature(read_initializer))]
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

use serde::Deserialize;
use std::{env, ffi::OsString, iter, net::SocketAddr, path, process};

use constellation_internal::{Envs, Format};

const USAGE: &str = "Run a binary on a constellation cluster.

USAGE:
    deploy [options] <host> <binary> [--] [<args>]...

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
	arg_args: Vec<String>, // OsString
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
	let bridge_address: SocketAddr = args.arg_host.parse().unwrap();
	let path = args.arg_binary;
	let args: Vec<OsString> = iter::once(OsString::from(&path))
		.chain(args.arg_args.into_iter().map(OsString::from))
		.collect();
	let vars: Vec<(OsString, OsString)> = env::vars_os().collect();
	constellation::deploy(bridge_address, &path, format, args, vars);
}
