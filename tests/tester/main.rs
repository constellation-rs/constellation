//! This is the testsuite for `deploy`. It runs the tests in `test/src/bin/` first directly, and then deployed to a local `fabric`.
//!
//! At the top of each test is some JSON, denoted with the special comment syntax `//=`.
//! `output` is a hashmap of file descriptor to a regex of expected output. As it is a regex ensure that any literal `\.+*?()|[]{}^$#&-~` are escaped.

#![feature(allocator_api, try_from, nll)]
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
	clippy::if_not_else,
	clippy::type_complexity,
	clippy::cast_possible_truncation,
	clippy::derive_hash_xor_eq
)]

extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate constellation_internal;
extern crate crossbeam;
extern crate escargot;
extern crate itertools;
extern crate multiset;
extern crate regex;
extern crate serde_json;

mod ext;

use constellation_internal::{ExitStatus, FabricOutputEvent};
use std::{
	collections::{HashMap, HashSet}, env, fs, hash, io::{self, BufRead}, iter, net, os, path::{Path, PathBuf}, process, str::{self, FromStr}, thread, time
};

const DEPLOY: &str = "src/bin/deploy.rs";
const FABRIC: &str = "src/bin/constellation/main.rs";
const BRIDGE: &str = "src/bin/bridge.rs";
const TESTS: &str = "tests/";
const SELF: &str = "tests/tester/main.rs";

const FABRIC_ADDR: &str = "127.0.0.1:12360";
const BRIDGE_ADDR: &str = "127.0.0.1:12340";

#[global_allocator]
static GLOBAL: std::alloc::System = std::alloc::System;

#[derive(PartialEq, Eq, Serialize, Debug)]
struct Output {
	output: HashMap<
		os::unix::io::RawFd,
		(ext::serialize_as_regex_string::SerializeAsRegexString, bool),
	>,
	#[serde(with = "ext::serde_multiset")]
	children: multiset::HashMultiSet<Output>,
	exit: ExitStatus,
}
impl hash::Hash for Output {
	fn hash<H>(&self, state: &mut H)
	where
		H: hash::Hasher,
	{
		let mut output = self.output.iter().collect::<Vec<_>>();
		output.sort_by(|&(ref a_fd, _), &(ref b_fd, _)| a_fd.cmp(b_fd));
		for output in output {
			output.hash(state);
		}
		// self.children.hash(state); // TODO
		self.exit.hash(state);
	}
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug)]
struct OutputTest {
	output: HashMap<os::unix::io::RawFd, (ext::serde_regex::SerdeRegex, bool)>,
	#[serde(with = "ext::serde_multiset")]
	children: multiset::HashMultiSet<OutputTest>,
	exit: ExitStatus,
}
impl hash::Hash for OutputTest {
	fn hash<H>(&self, state: &mut H)
	where
		H: hash::Hasher,
	{
		let mut output = self.output.iter().collect::<Vec<_>>();
		output.sort_by(|&(ref a_fd, _), &(ref b_fd, _)| a_fd.cmp(b_fd));
		for output in output {
			output.hash(state);
		}
		// self.children.hash(state); // TODO
		self.exit.hash(state);
	}
}
impl OutputTest {
	fn is_match(&self, output: &Output) -> bool {
		if self.exit != output.exit
			|| self.output.len() != output.output.len()
			|| self.children.len() != output.children.len()
		{
			return false;
		}
		for (_, test, output) in ext::hashmap::intersection(&self.output, &output.output) {
			if test.1 != output.1 || !test.0.is_match(&(output.0).0) {
				return false;
			}
		}
		let mut check = (0..self.children.len())
			.map(|_| (false, false))
			.collect::<Vec<_>>();
		for (i, test) in self.children.iter().enumerate() {
			for (j, output) in output.children.iter().enumerate() {
				if !(check[i].0 && check[j].1) && test.is_match(output) {
					check[i].0 = true;
					check[j].1 = true;
				}
			}
		}
		if !check.into_iter().all(|(a, b)| a && b) {
			return false;
		}
		true
	}
}

fn parse_output(output: &process::Output) -> Result<Output, Option<serde_json::Error>> {
	if !output.stderr.is_empty() {
		return Err(None);
		// println!("{}", std::str::from_utf8(&output.stderr).unwrap()); // Useful if FORWARD_STDERR is disabled
	}
	let mut log = HashMap::new();
	let mut top = None;
	for message in serde_json::Deserializer::from_slice(&output.stdout)
		.into_iter::<constellation_internal::DeployOutputEvent>()
	{
		match message? {
			constellation_internal::DeployOutputEvent::Output(a, b, c) => {
				if top.is_none() {
					top = Some(a);
					let _ = log.insert(a, (HashMap::new(), Vec::new(), None));
				}
				let output = log
					.get_mut(&a)
					.unwrap()
					.0
					.entry(b)
					.or_insert((Vec::new(), false));
				assert!(!output.1);
				if !c.is_empty() {
					output.0.extend(c);
				} else {
					output.1 = true;
				}
			}
			constellation_internal::DeployOutputEvent::Spawn(a, b) => {
				if top.is_none() {
					top = Some(a);
					let _ = log.insert(a, (HashMap::new(), Vec::new(), None));
				}
				log.get_mut(&a).unwrap().1.push(b);
				let x = log.insert(b, (HashMap::new(), Vec::new(), None));
				assert!(x.is_none());
			}
			constellation_internal::DeployOutputEvent::Exit(a, b) => {
				if top.is_none() {
					top = Some(a);
					let _ = log.insert(a, (HashMap::new(), Vec::new(), None));
				}
				log.get_mut(&a).unwrap().2 = Some(b);
			}
		}
	}
	let top = top.unwrap();
	Ok(treeize(log.remove(&top).unwrap(), &mut log))
}

fn treeize(
	output: (
		HashMap<os::unix::io::RawFd, (Vec<u8>, bool)>,
		Vec<constellation_internal::Pid>,
		Option<ExitStatus>,
	),
	nodes: &mut HashMap<
		constellation_internal::Pid,
		(
			HashMap<os::unix::io::RawFd, (Vec<u8>, bool)>,
			Vec<constellation_internal::Pid>,
			Option<ExitStatus>,
		),
	>,
) -> Output {
	Output {
		output: output
			.0
			.into_iter()
			// .chain(iter::once((2,(vec![],true)))) // Useful if FORWARD_STDERR is disabled
			.map(|(k, (v, v1))| {
				(
					k,
					(
						ext::serialize_as_regex_string::SerializeAsRegexString(v),
						v1,
					),
				)
			})
			.collect(),
		children: output
			.1
			.into_iter()
			.map(|pid| treeize(nodes.remove(&pid).unwrap(), nodes))
			.collect(),
		exit: output.2.unwrap(),
	}
}

fn main() {
	let start = time::Instant::now();
	let _ = thread::Builder::new()
		.spawn(move || loop {
			thread::sleep(time::Duration::new(10, 0));
			println!("{:?}", start.elapsed());
		})
		.unwrap();
	let current_dir = env::current_dir().unwrap();
	let mut products = HashMap::new();
	let mut args = env::args().skip(1);
	let iterations = args
		.next()
		.and_then(|arg| arg.parse::<usize>().ok())
		.unwrap_or(10);
	let args = iter::once(String::from("build"))
		.chain(iter::once(String::from("--tests")))
		.chain(iter::once(String::from("--message-format=json")))
		.chain(iter::once(format!("--target={}", escargot::CURRENT_TARGET)))
		.chain(args)
		.collect::<Vec<_>>();
	let output = process::Command::new("cargo")
		.args(&args)
		.stderr(process::Stdio::inherit())
		.output()
		.expect("Failed to invoke cargo");
	if !output.status.success() {
		panic!("cargo build failed");
	}
	for message in serde_json::Deserializer::from_slice(&output.stdout)
		.into_iter::<constellation_internal::cargo_metadata::Message>()
	{
		if let constellation_internal::cargo_metadata::Message::CompilerArtifact { artifact } =
			message.unwrap_or_else(|_| {
				panic!(
					"Failed to parse output of cargo {}",
					itertools::join(args.iter(), " ")
				)
			}) {
			if let Ok(path) = PathBuf::from(&artifact.target.src_path).strip_prefix(&current_dir) {
				if (artifact.target.kind == vec![String::from("bin")] && !artifact.profile.test)
					|| artifact.target.kind == vec![String::from("test")]
				{
					// println!("{:#?}", artifact);
					// assert_eq!(artifact.filenames.len(), 1, "{:?}", artifact);
					let x = products.insert(
						path.to_owned(),
						artifact.filenames.into_iter().nth(0).unwrap(),
					);
					assert!(x.is_none());
				}
			}
		}
	}

	let (deploy, fabric, bridge) = (
		&products[Path::new(DEPLOY)],
		&products[Path::new(FABRIC)],
		&products[Path::new(BRIDGE)],
	);

	if net::TcpStream::connect(FABRIC_ADDR).is_ok() {
		panic!("Service already running on FABRIC_ADDR {}", FABRIC_ADDR);
	}
	if net::TcpStream::connect(BRIDGE_ADDR).is_ok() {
		panic!("Service already running on BRIDGE_ADDR {}", BRIDGE_ADDR);
	}
	let mut fabric = process::Command::new(fabric)
		.args(&[
			"master",
			&net::SocketAddr::from_str(FABRIC_ADDR)
				.unwrap()
				.ip()
				.to_string(),
			FABRIC_ADDR,
			"4GiB",
			"4",
			bridge.to_str().unwrap(),
			BRIDGE_ADDR,
		])
		.stdin(process::Stdio::null())
		.stdout(process::Stdio::piped())
		.stderr(process::Stdio::piped())
		.spawn()
		.unwrap();
	let start_ = time::Instant::now();
	while net::TcpStream::connect(FABRIC_ADDR).is_err() {
		// TODO: parse output rather than this loop and timeout
		if start_.elapsed() > time::Duration::new(2, 0) {
			panic!("Fabric not up within 2s");
		}
		thread::sleep(std::time::Duration::new(0, 1_000_000));
	}
	let mut fabric_stdout = serde_json::StreamDeserializer::new(serde_json::de::IoRead::new(
		fabric.stdout.take().unwrap(),
	));
	let mut fabric_stderr = fabric.stderr.take().unwrap();
	let fabric_stderr = thread::spawn(move || {
		let mut none = true;
		loop {
			use std::io::Read;
			let mut stderr = vec![0; 1024];
			// println!("awaiting stderr");
			let n = fabric_stderr.read(&mut stderr).unwrap();
			// println!("got stderr {}", n);
			if n == 0 {
				break;
			}
			none = false;
			stderr.truncate(n);
			println!("fab stderr: {:?}", String::from_utf8(stderr).unwrap());
		}
		none
	});
	let start_ = time::Instant::now();
	while net::TcpStream::connect(BRIDGE_ADDR).is_err() {
		// TODO: parse output rather than this loop and timeout
		if start_.elapsed() > time::Duration::new(10, 0) {
			panic!("Bridge not up within 10s");
		}
		thread::sleep(std::time::Duration::new(0, 1_000_000));
	}
	let _bridge_pid: FabricOutputEvent = fabric_stdout.next().unwrap().unwrap();

	let mut products = products
		.iter()
		.filter(|&(src, _bin)| src.starts_with(Path::new(TESTS)) && src != Path::new(SELF))
		.collect::<Vec<_>>();
	products.sort_by(|&(ref a_src, _), &(ref b_src, _)| a_src.cmp(b_src));

	let (mut succeeded, mut failed) = (0, 0);
	for (src, bin) in products {
		if src != Path::new("tests/xx.rs") {
			continue;
		}
		println!("{}", src.display());
		let file: Result<OutputTest, _> = serde_json::from_str(
			&io::BufReader::new(fs::File::open(src).unwrap())
				.lines()
				.map(|x| x.unwrap())
				.take_while(|x| x.get(0..3) == Some("//="))
				.flat_map(|x| ext::string::Chars::new(x).skip(3))
				.collect::<String>(),
		);
		let mut x = |command: &mut process::Command, deployed: bool| {
			let result = crossbeam::thread::scope(|scope| {
				if deployed {
					fn count_processes(output_test: &OutputTest) -> usize {
						1 + output_test
							.children
							.iter()
							.map(count_processes)
							.sum::<usize>()
					}
					let processes = count_processes(file.as_ref().unwrap());
					let fabric_stdout = &mut fabric_stdout;
					let _ = scope.spawn(move || {
						let mut pids = HashSet::new();
						for _ in 0..processes * 2 {
							match fabric_stdout.next().unwrap().unwrap() {
								FabricOutputEvent::Init(a, b) => {
									let x = pids.insert((a, b));
									assert!(x);
								}
								FabricOutputEvent::Exit(a, b) => {
									let x = pids.remove(&(a, b));
									assert!(x);
								}
							}
						}
						assert!(pids.is_empty(), "{:?}", pids);
					});
				}
				command.output().unwrap()
			});
			let output = parse_output(&result);
			if output.is_err()
				|| file.is_err()
				|| !file.as_ref().unwrap().is_match(output.as_ref().unwrap())
			{
				println!("Error in {:?}", src);
				match file {
					Ok(ref file) => println!(
						"Documented:\n{}",
						serde_json::to_string_pretty(file).unwrap()
					),
					Err(ref e) => println!("Documented:\nInvalid result JSON: {:?}\n", e),
				}
				match output {
					Ok(ref output) => {
						println!("Actual:\n{}", serde_json::to_string_pretty(output).unwrap())
					}
					Err(ref e) => println!("Actual:\nFailed to parse: {:?}\n{:?}", result, e),
				}
				failed += 1;
			} else {
				succeeded += 1;
			}
		};
		println!("  native");
		for i in 0..iterations {
			println!("    {}", i);
			x(
				process::Command::new(bin)
					.env_remove("CONSTELLATION_VERSION")
					.env("CONSTELLATION_FORMAT", "json"),
				false,
			);
		}
		println!("  deployed");
		for i in 0..iterations {
			println!("    {}", i);
			x(
				process::Command::new(deploy)
					.env_remove("CONSTELLATION_VERSION")
					.env_remove("CONSTELLATION_FORMAT")
					.args(&["--format=json", BRIDGE_ADDR, bin.to_str().unwrap()]),
				true,
			);
		}
	}

	fabric.kill().unwrap();
	let x = fabric_stdout.next();
	assert!(x.is_none());
	let x = fabric_stderr.join().unwrap();
	assert!(x);

	println!(
		"{}/{} succeeded in {:?}",
		succeeded,
		succeeded + failed,
		start.elapsed()
	);
	if failed > 0 {
		process::exit(1);
	}
}
