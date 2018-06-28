//! This is the testsuite for `deploy`. It runs the tests in `test/src/bin/` first directly, and then deployed to a local `fabric`.
//!
//! At the top of each test is some JSON, denoted with the special comment syntax `//=`.
//! `output` is a hashmap of file descriptor to a regex of expected output. As it is a regex ensure that any literal `\.+*?()|[]{}^$#&-~` are escaped.

#![feature(global_allocator, allocator_api)]
#![deny(missing_docs, warnings, deprecated)]

extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate deploy_common;
extern crate either;
extern crate itertools;
extern crate multiset;
extern crate regex;
extern crate serde_json;

mod ext;

use std::{
	collections::HashMap,
	env, fs, hash,
	io::{self, BufRead},
	iter, os,
	path::{Path, PathBuf},
	process, str, thread, time,
};

const DEPLOY: &'static str = "deploy/src/bin/deploy.rs";
const FABRIC: &'static str = "fabric/src/main.rs";
const BRIDGE: &'static str = "deploy/src/bin/bridge.rs";
const TESTS: &'static str = "test/src/bin/";

const FABRIC_ADDR: &'static str = "127.0.0.1:12360";
const BRIDGE_ADDR: &'static str = "127.0.0.1:12340";

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
	exit: either::Either<u8, deploy_common::Signal>,
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
	exit: either::Either<u8, deploy_common::Signal>,
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
	if output.stderr.len() != 0 {
		return Err(None);
	}
	let mut log = HashMap::new();
	let mut top = None;
	for message in serde_json::Deserializer::from_slice(&output.stdout)
		.into_iter::<deploy_common::DeployOutputEvent>()
	{
		match message? {
			deploy_common::DeployOutputEvent::Output(a, b, c) => {
				if let None = top {
					top = Some(a);
					log.insert(a, (HashMap::new(), Vec::new(), None));
				}
				let output = log
					.get_mut(&a)
					.unwrap()
					.0
					.entry(b)
					.or_insert((Vec::new(), false));
				assert!(!output.1);
				if c.len() > 0 {
					output.0.extend(c);
				} else {
					output.1 = true;
				}
			}
			deploy_common::DeployOutputEvent::Spawn(a, b) => {
				if let None = top {
					top = Some(a);
					log.insert(a, (HashMap::new(), Vec::new(), None));
				}
				log.get_mut(&a).unwrap().1.push(b);
				let x = log.insert(b, (HashMap::new(), Vec::new(), None));
				assert!(x.is_none());
			}
			deploy_common::DeployOutputEvent::Exit(a, b) => {
				if let None = top {
					top = Some(a);
					log.insert(a, (HashMap::new(), Vec::new(), None));
				}
				log.get_mut(&a).unwrap().2 = Some(b);
			}
		}
	}
	let top = top.unwrap();
	fn treeize(
		output: (
			HashMap<os::unix::io::RawFd, (Vec<u8>, bool)>,
			Vec<deploy_common::Pid>,
			Option<either::Either<u8, deploy_common::Signal>>,
		),
		nodes: &mut HashMap<
			deploy_common::Pid,
			(
				HashMap<os::unix::io::RawFd, (Vec<u8>, bool)>,
				Vec<deploy_common::Pid>,
				Option<either::Either<u8, deploy_common::Signal>>,
			),
		>,
	) -> Output {
		Output {
			output: output
				.0
				.into_iter()
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
	Ok(treeize(log.remove(&top).unwrap(), &mut log))
}

fn main() {
	let start = time::Instant::now();
	thread::Builder::new()
		.spawn(move || loop {
			thread::sleep(time::Duration::new(10, 0));
			println!("{:?}", start.elapsed());
		})
		.unwrap();
	let current_dir = env::current_dir().unwrap();
	let mut products = HashMap::new();
	let args = iter::once(String::from("build"))
		.chain(iter::once(String::from("--message-format=json")))
		.chain(env::args().skip(1))
		.collect::<Vec<_>>();
	let output = process::Command::new("cargo")
		.args(&args)
		.stderr(process::Stdio::inherit())
		.output()
		.expect("Failed to invoke cargo");
	for message in serde_json::Deserializer::from_slice(&output.stdout)
		.into_iter::<deploy_common::cargo_metadata::Message>()
	{
		if let deploy_common::cargo_metadata::Message::CompilerArtifact { artifact } = message
			.unwrap_or_else(|_| {
				panic!(
					"Failed to parse output of cargo {}",
					itertools::join(args.iter(), " ")
				)
			}) {
			if artifact.target.kind == vec![String::from("bin")] {
				if let Ok(path) =
					PathBuf::from(&artifact.target.src_path).strip_prefix(&current_dir)
				{
					assert_eq!(artifact.filenames.len(), 1);
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

	if let Ok(_) = std::net::TcpStream::connect(FABRIC_ADDR) {
		panic!("Service already running on FABRIC_ADDR {}", FABRIC_ADDR);
	}
	if let Ok(_) = std::net::TcpStream::connect(BRIDGE_ADDR) {
		panic!("Service already running on BRIDGE_ADDR {}", BRIDGE_ADDR);
	}
	let mut fabric = process::Command::new(fabric)
		.args(&[
			"master",
			FABRIC_ADDR,
			"4GiB",
			"4",
			bridge.to_str().unwrap(),
			BRIDGE_ADDR,
		])
		.stdin(process::Stdio::null())
		.stdout(process::Stdio::null())
		.stderr(process::Stdio::null())
		.spawn()
		.unwrap();
	let start_ = time::Instant::now();
	while let Err(_) = std::net::TcpStream::connect(FABRIC_ADDR) {
		// TODO: parse output rather than this loop and timeout
		if start_.elapsed() > time::Duration::new(2, 0) {
			panic!("Fabric not up within 2s");
		}
		thread::sleep(std::time::Duration::new(0, 1_000_000));
	}
	let start_ = time::Instant::now();
	while let Err(_) = std::net::TcpStream::connect(BRIDGE_ADDR) {
		// TODO: parse output rather than this loop and timeout
		if start_.elapsed() > time::Duration::new(10, 0) {
			panic!("Bridge not up within 10s");
		}
		thread::sleep(std::time::Duration::new(0, 1_000_000));
	}

	let mut products = products
		.iter()
		.filter(|&(src, _bin)| src.starts_with(Path::new(TESTS)))
		.collect::<Vec<_>>();
	products.sort_by(|&(ref a_src, _), &(ref b_src, _)| a_src.cmp(b_src));

	let (mut succeeded, mut failed) = (0, 0);
	for (src, bin) in products {
		println!("{}", src.display());
		let file: Result<OutputTest, _> = serde_json::from_str(
			&io::BufReader::new(fs::File::open(src).unwrap())
				.lines()
				.map(|x| x.unwrap())
				.take_while(|x| x.get(0..3) == Some("//="))
				.flat_map(|x| ext::string::Chars::new(x).skip(3))
				.collect::<String>(),
		);
		let mut x = |command: &mut process::Command| {
			let result = command.output().unwrap();
			let output = parse_output(&result);
			if output.is_err()
				|| file.is_err()
				|| !file.as_ref().unwrap().is_match(output.as_ref().unwrap())
			{
				println!("Error in {:?}", src);
				match &file {
					&Ok(ref file) => println!(
						"Documented:\n{}",
						serde_json::to_string_pretty(file).unwrap()
					),
					&Err(ref e) => println!("Documented:\nInvalid result JSON: {:?}\n", e),
				}
				match &output {
					&Ok(ref output) => {
						println!("Actual:\n{}", serde_json::to_string_pretty(output).unwrap())
					}
					&Err(ref e) => println!("Actual:\nFailed to parse: {:?}\n{:?}", result, e),
				}
				failed += 1;
			} else {
				succeeded += 1;
			}
		};
		println!("  native");
		for i in 0..10 {
			println!("    {}", i);
			x(process::Command::new(bin)
				.env_remove("DEPLOY_VERSION")
				.env("DEPLOY_FORMAT", "json"));
		}
		println!("  deployed");
		for i in 0..10 {
			println!("    {}", i);
			x(process::Command::new(deploy)
				.env_remove("DEPLOY_VERSION")
				.env_remove("DEPLOY_FORMAT")
				.args(&["--format=json", BRIDGE_ADDR, bin.to_str().unwrap()]));
		}
	}

	fabric.kill().unwrap();

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
