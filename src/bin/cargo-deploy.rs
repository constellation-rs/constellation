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

use clap::{crate_version, App, AppSettings, Arg, ArgMatches, SubCommand};
use std::{
	convert::TryInto, env, ffi::{OsStr, OsString}, iter, net::SocketAddr, process
};

use constellation_internal::Format;

fn main() {
	let args = cli().get_matches();
	let args = args.subcommand_matches("deploy").unwrap();
	let host: SocketAddr = args.value_of("host").unwrap().parse().unwrap();
	let forward_args: Vec<&OsStr> = args.values_of_os("args").unwrap_or_default().collect();
	let output = cargo(args)
		.stderr(process::Stdio::inherit())
		.output()
		.expect("Failed to invoke cargo");
	if !output.status.success() {
		process::exit(output.status.code().unwrap_or(101));
	}
	let mut bin = Vec::new();
	for message in serde_json::Deserializer::from_slice(&output.stdout).into_iter() {
		if let cargo_metadata::Message::CompilerArtifact(artifact) =
			message.unwrap_or_else(|_| panic!("Failed to parse output of cargo"))
		{
			if artifact.target.kind == vec![String::from("bin")]
				|| artifact.target.kind == vec![String::from("example")]
			{
				bin.push((
					artifact.target.name,
					artifact.filenames.into_iter().next().unwrap(),
				));
				// We're assuming the first filename is the binary – .dSYM etc seem to always be second?
			}
		}
	}
	if bin.len() > 1 {
		let names = bin
			.into_iter()
			.map(|(target_name, _)| target_name)
			.collect::<Vec<_>>();
		println!(
			"`cargo deploy` could not determine which binary to run. \
			 Use the `--bin` option to specify a binary.\n\
			 available binaries: {}",
			names.join(", ")
		); // , or the `default-run` manifest key // TODO: work out best way to get this / behave same as cargo run
		process::exit(1);
	} else if bin.is_empty() {
		println!("a bin target must be available for `cargo deploy`");
		process::exit(1);
	}
	let path = bin.into_iter().next().unwrap().1;
	let args: Vec<OsString> = iter::once(OsString::from(&path))
		.chain(forward_args.into_iter().map(ToOwned::to_owned))
		.collect();
	let vars: Vec<(OsString, OsString)> = env::vars_os().collect();
	let format = Format::Human;
	constellation::deploy(host, &path, format, args, vars);
}

fn cli<'a, 'b>() -> App<'a, 'b> {
	// https://github.com/rust-lang/cargo/blob/7059559d71de3fffe8c8cb81e32f323454aa96c5/src/bin/cargo/cli.rs#L205-L277
	// https://github.com/rust-lang/cargo/blob/982622252a64d7c526c04a244f1a81523dc9ae54/src/bin/cargo/commands/run.rs
	App::new("cargo")
		.bin_name("cargo")
		.settings(&[
			AppSettings::UnifiedHelpMessage,
			AppSettings::DeriveDisplayOrder,
			AppSettings::SubcommandRequired,
		])
		.arg(
			Arg::opt(
				"verbose",
				"Use verbose output (-vv very verbose/build.rs output)",
			)
			.short("v")
			.multiple(true)
			.global(true),
		)
		.arg(
			Arg::opt("color", "Coloring: auto, always, never")
				.value_name("WHEN")
				.global(true),
		)
		.arg(Arg::opt("frozen", "Require Cargo.lock and cache are up to date").global(true))
		.arg(Arg::opt("locked", "Require Cargo.lock is up to date").global(true))
		.arg(Arg::opt("offline", "Run without accessing the network").global(true))
		.arg(
			Arg::multi_opt("config", "KEY=VALUE", "Override a configuration value")
				.global(true)
				.hidden(true),
		)
		.arg(
			Arg::with_name("unstable-features")
				.help("Unstable (nightly-only) flags to Cargo, see 'cargo -Z help' for details")
				.short("Z")
				.value_name("FLAG")
				.multiple(true)
				.number_of_values(1)
				.global(true),
		)
		.subcommand(
			SubCommand::with_name("deploy")
				.settings(&[
					AppSettings::UnifiedHelpMessage,
					AppSettings::DeriveDisplayOrder,
					AppSettings::DontCollapseArgsInUsage,
					AppSettings::TrailingVarArg,
				])
				.version(crate_version!())
				.about("Run a binary or example of the local package on a constellation cluster")
				// .arg(Arg::opt("quiet", "No output printed to stdout").short("q"))
				.arg(
					Arg::with_name("host")
						.help("Constellation cluster node to connect to (e.g. 10.0.0.1:8888)")
						.required(true)
						.validator(|host| {
							host.parse::<SocketAddr>()
								.map(drop)
								.map_err(|err| err.to_string())
						}),
				)
				.arg(Arg::with_name("args").multiple(true))
				.args(&Arg::targets_bin_example(
					"Name of the bin target to run",
					"Name of the example target to run",
				))
				.arg(Arg::package("Package with the target to run"))
				.arg(Arg::jobs())
				.arg(Arg::release(
					"Build artifacts in release mode, with optimizations",
				))
				.arg(Arg::profile("Build artifacts with the specified profile"))
				.args(&Arg::features())
				.arg(Arg::target_triple("Build for the target triple"))
				.arg(Arg::target_dir())
				.arg(Arg::manifest_path())
				// .arg(Arg::message_format())
				.after_help(
					"\
If neither `--bin` nor `--example` are given, then if the package only has one
bin target it will be run. Otherwise `--bin` specifies the bin target to run,
and `--example` specifies the example target to run. At most one of `--bin` or
`--example` can be provided.

All the arguments following the two dashes (`--`) are passed to the binary to
run. If you're passing arguments to both Cargo and the binary, the ones after
`--` go to the binary, the ones before go to Cargo.
",
				),
		)
}

fn cargo(args: &ArgMatches) -> process::Command {
	let verbose: u64 = args.occurrences_of("verbose");
	let color: Option<&str> = args.value_of("color");
	let frozen: bool = args.is_present("frozen");
	let locked: bool = args.is_present("locked");
	let offline: bool = args.is_present("offline");
	let config: Vec<&str> = args.values_of("config").unwrap_or_default().collect();
	let unstable_features: Vec<&OsStr> = args
		.values_of_os("unstable-features")
		.unwrap_or_default()
		.collect();
	let bin: Vec<&str> = args.values_of("bin").unwrap_or_default().collect();
	let example: Vec<&str> = args.values_of("example").unwrap_or_default().collect();
	let package: Vec<&str> = args.values_of("package").unwrap_or_default().collect();
	let jobs: Option<&str> = args.value_of("jobs");
	let release: bool = args.is_present("release");
	let profile: Option<&str> = args.value_of("profile");
	let features: Vec<&str> = args.values_of("features").unwrap_or_default().collect();
	let all_features: bool = args.is_present("all-features");
	let no_default_features: bool = args.is_present("no-default-features");
	let target: Option<&str> = args.value_of("target");
	let target_dir: Option<&str> = args.value_of("target-dir");
	let manifest_path: Option<&str> = args.value_of("manifest-path");
	// let mut args: Vec<String> = Vec::new();
	let mut cargo = process::Command::new("cargo");
	let _ = cargo.arg("build");
	let _ = cargo.arg("--message-format=json");
	if verbose > 0 {
		let _ = cargo.arg(format!("-{}", "v".repeat(verbose.try_into().unwrap())));
	}
	if let Some(color) = color {
		let _ = cargo.arg(format!("--color={}", color));
	}
	if frozen {
		let _ = cargo.arg("--frozen");
	}
	if locked {
		let _ = cargo.arg("--locked");
	}
	if offline {
		let _ = cargo.arg("--offline");
	}
	for config in config {
		let _ = cargo.arg(format!("--config={}", config));
	}
	for unstable_features in unstable_features {
		let mut arg = OsString::from("-Z");
		arg.push(unstable_features);
		let _ = cargo.arg(arg);
	}
	for bin in bin {
		let _ = cargo.arg(format!("--bin={}", bin));
	}
	for example in example {
		let _ = cargo.arg(format!("--example={}", example));
	}
	for package in package {
		let _ = cargo.arg(format!("--package={}", package));
	}
	if let Some(jobs) = jobs {
		let _ = cargo.arg(format!("--jobs={}", jobs));
	}
	if release {
		let _ = cargo.arg("--release");
	}
	if let Some(profile) = profile {
		let _ = cargo.arg(format!("--profile={}", profile));
	}
	for features in features {
		let _ = cargo.arg(format!("--features={}", features));
	}
	if all_features {
		let _ = cargo.arg("--all-features");
	}
	if no_default_features {
		let _ = cargo.arg("--no-default-features");
	}
	if let Some(target) = target {
		let _ = cargo.arg(format!("--target={}", target));
	}
	if let Some(target_dir) = target_dir {
		let _ = cargo.arg(format!("--target-dir={}", target_dir));
	}
	if let Some(manifest_path) = manifest_path {
		let _ = cargo.arg(format!("--manifest-path={}", manifest_path));
	}
	cargo
}

// https://github.com/rust-lang/cargo/blob/7059559d71de3fffe8c8cb81e32f323454aa96c5/src/cargo/util/command_prelude.rs
trait ArgExt: Sized {
	fn opt(name: &'static str, help: &'static str) -> Self;
	fn optional_multi_opt(name: &'static str, value_name: &'static str, help: &'static str)
		-> Self;
	fn multi_opt(name: &'static str, value_name: &'static str, help: &'static str) -> Self;
	fn targets_bin_example(bin: &'static str, example: &'static str) -> [Self; 2];
	fn package(package: &'static str) -> Self;
	fn jobs() -> Self;
	fn release(release: &'static str) -> Self;
	fn profile(profile: &'static str) -> Self;
	fn features() -> [Self; 3];
	fn target_triple(target: &'static str) -> Self;
	fn target_dir() -> Self;
	fn manifest_path() -> Self;
}
impl<'a, 'b> ArgExt for Arg<'a, 'b> {
	fn opt(name: &'static str, help: &'static str) -> Self {
		Arg::with_name(name).long(name).help(help)
	}
	fn optional_multi_opt(
		name: &'static str, value_name: &'static str, help: &'static str,
	) -> Self {
		Self::opt(name, help)
			.value_name(value_name)
			.multiple(true)
			.min_values(0)
			.number_of_values(1)
	}
	fn multi_opt(name: &'static str, value_name: &'static str, help: &'static str) -> Self {
		// Note that all `.multiple(true)` arguments in Cargo should specify
		// `.number_of_values(1)` as well, so that `--foo val1 val2` is
		// *not* parsed as `foo` with values ["val1", "val2"].
		// `number_of_values` should become the default in clap 3.
		Self::opt(name, help)
			.value_name(value_name)
			.multiple(true)
			.number_of_values(1)
	}
	fn targets_bin_example(bin: &'static str, example: &'static str) -> [Self; 2] {
		[
			Self::optional_multi_opt("bin", "NAME", bin),
			Self::optional_multi_opt("example", "NAME", example),
		]
	}
	fn package(package: &'static str) -> Self {
		Self::opt("package", package).short("p").value_name("SPEC")
	}
	fn jobs() -> Self {
		Self::opt("jobs", "Number of parallel jobs, defaults to # of CPUs")
			.short("j")
			.value_name("N")
	}
	fn release(release: &'static str) -> Self {
		Self::opt("release", release)
	}
	fn profile(profile: &'static str) -> Self {
		Self::opt("profile", profile).value_name("PROFILE-NAME")
	}
	fn features() -> [Self; 3] {
		[
			Self::multi_opt(
				"features",
				"FEATURES",
				"Space-separated list of features to activate",
			),
			Self::opt("all-features", "Activate all available features"),
			Self::opt(
				"no-default-features",
				"Do not activate the `default` feature",
			),
		]
	}
	fn target_triple(target: &'static str) -> Self {
		Self::opt("target", target).value_name("TRIPLE")
	}
	fn target_dir() -> Self {
		Self::opt("target-dir", "Directory for all generated artifacts").value_name("DIRECTORY")
	}
	fn manifest_path() -> Self {
		Self::opt("manifest-path", "Path to Cargo.toml").value_name("PATH")
	}
}
