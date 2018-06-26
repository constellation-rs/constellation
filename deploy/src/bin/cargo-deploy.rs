extern crate cargo_metadata;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;

use std::*;

mod ext {
	pub mod cargo_metadata {
		use cargo_metadata;
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
			pub target: cargo_metadata::Target, // https://github.com/rust-lang/cargo/blob/c24a09772c2c1cb315970dbc721f2a42d4515f21/src/cargo/core/manifest.rs#L188
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
}

fn main() {
	let mut args = env::args().skip(2);
	let deploy_args = args.collect::<Vec<_>>();
	let addr: net::SocketAddr = deploy_args[0].parse().unwrap();
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
		.into_iter::<ext::cargo_metadata::Message>()
	{
		if let ext::cargo_metadata::Message::CompilerArtifact { artifact } =
			message.unwrap_or_else(|_| {
				panic!(
					"Failed to parse output of cargo" // {}",
					                                  // itertools::join(args.iter(), " ")
				)
			}) {
			if artifact.target.kind == vec![String::from("bin")] {
				assert_eq!(artifact.filenames.len(), 1);
				assert!(bin.is_none());
				bin = Some(artifact.filenames.into_iter().nth(0).unwrap());
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
