//! # `cargo deploy`
//! Run a binary on a constellation cluster
//!
//! ## Usage
//! ```text
//! cargo deploy [options] <host> [--] [<args>]...
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

use std::{env, iter, net, process};

fn main() {
	let args = env::args().skip(2);
	let deploy_args = args.collect::<Vec<_>>();
	let _addr: net::SocketAddr = deploy_args[0].parse().unwrap();
	let args = iter::once(String::from("build"))
		.chain(iter::once(String::from("--message-format=json")))
		.chain(deploy_args.clone().into_iter().skip(1))
		.collect::<Vec<_>>();
	let output = process::Command::new("cargo")
		.args(&args)
		.stderr(process::Stdio::inherit())
		.output()
		.expect("Failed to invoke cargo");
	let mut bin = None;
	for message in serde_json::Deserializer::from_slice(&output.stdout)
		.into_iter::<constellation_internal::cargo_metadata::Message>()
	{
		if let constellation_internal::cargo_metadata::Message::CompilerArtifact { artifact } =
			message.unwrap_or_else(|_| {
				panic!("Failed to parse output of cargo") // itertools::join(args.iter(), " ")
			}) {
			if artifact.target.kind == vec![String::from("bin")] {
				assert_eq!(artifact.filenames.len(), 1);
				assert!(bin.is_none());
				bin = Some(artifact.filenames.into_iter().next().unwrap());
			}
		}
	}
	let bin = bin.expect("No binary found");
	process::exit(
		process::Command::new("deploy")
			.args(&deploy_args)
			.arg(&bin)
			.stdout(process::Stdio::inherit())
			.stderr(process::Stdio::inherit())
			.output()
			.expect("Failed to invoke deploy")
			.status
			.code()
			.unwrap_or(101),
	);
}
