//! This is the testsuite for `deploy`. It runs the tests in `test/src/bin/` first directly, and then deployed to a local `fabric`.
//!
//! At the top of each test is some JSON, denoted with the special comment syntax `//=`.
//! `output` is a hashmap of file descriptor to a regex of expected output. As it is a regex ensure that any literal `\.+*?()|[]{}^$#&-~` are escaped.

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

mod ext;

use multiset::HashMultiSet;
use serde::{Deserialize, Serialize};
use std::{
	collections::HashMap, env, fmt, fs::File, hash, io::{self, BufRead, BufReader}, iter, net::TcpStream, path::{Path, PathBuf}, process, str, thread, time::{self, Duration}
};
use systemstat::{saturating_sub_bytes, Platform, System};

use constellation_internal::{ExitStatus, Fd};
use ext::serialize_as_regex_string::SerializeAsRegexString;

const DEPLOY: &str = "src/bin/deploy.rs";
const FABRIC: &str = "src/bin/constellation/main.rs";
const TESTS: &str = "tests/";
const SELF: &str = "tests/tester/main.rs";

const FABRIC_ADDR: &str = "127.0.0.1:12360";
const BRIDGE_ADDR: &str = "127.0.0.1:12340";

const FORWARD_STDERR: bool = true;

#[derive(PartialEq, Eq, Serialize, Debug)]
struct Output {
	output: HashMap<Fd, (SerializeAsRegexString, bool)>,
	#[serde(with = "ext::serde_multiset")]
	children: HashMultiSet<Output>,
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
struct OutputTest {
	output: HashMap<Fd, (ext::serde_regex::SerdeRegex, bool)>,
	#[serde(with = "ext::serde_multiset")]
	children: HashMultiSet<OutputTest>,
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
	if !FORWARD_STDERR {
		println!("stderr: {:?}", str::from_utf8(&output.stderr).unwrap());
	} else if !output.stderr.is_empty() {
		return Err(None);
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

fn remove_stderr(mut output_test: OutputTest) -> OutputTest {
	let _stderr = output_test.output.remove(&2).unwrap();
	let mut children = output_test.children;
	let mut new_children = HashMultiSet::new();
	while !children.is_empty() {
		let key = children.distinct_elements().next().unwrap();
		let count = children.count_of(key);
		assert_ne!(count, 0);
		let key = key.clone();
		children.remove_all(&key);
		new_children.insert_times(remove_stderr(key), count);
	}
	output_test.children = new_children;
	output_test
}

fn treeize(
	output: (
		HashMap<Fd, (Vec<u8>, bool)>,
		Vec<constellation_internal::Pid>,
		Option<ExitStatus>,
	),
	nodes: &mut HashMap<
		constellation_internal::Pid,
		(
			HashMap<Fd, (Vec<u8>, bool)>,
			Vec<constellation_internal::Pid>,
			Option<ExitStatus>,
		),
	>,
) -> Output {
	Output {
		output: output
			.0
			.into_iter()
			.map(|(k, (v, v1))| (k, (SerializeAsRegexString(v), v1)))
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
	std::env::set_var("RUST_BACKTRACE", "full");
	std::panic::set_hook(Box::new(|info| {
		eprintln!(
			"thread '{}' {}",
			thread::current().name().unwrap_or("<unnamed>"),
			info
		);
		eprintln!("{:?}", backtrace::Backtrace::new());
		std::process::abort();
	}));
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

	let (deploy, fabric) = (&products[Path::new(DEPLOY)], &products[Path::new(FABRIC)]);

	if TcpStream::connect(FABRIC_ADDR).is_ok() {
		panic!("Service already running on FABRIC_ADDR {}", FABRIC_ADDR);
	}
	if TcpStream::connect(BRIDGE_ADDR).is_ok() {
		panic!("Service already running on BRIDGE_ADDR {}", BRIDGE_ADDR);
	}
	let mut fabric = process::Command::new(fabric)
		.args(&[
			"--format",
			"json",
			"-v",
			FABRIC_ADDR,
			FABRIC_ADDR,
			BRIDGE_ADDR,
			"4GiB",
			"4",
		])
		.stdin(process::Stdio::null())
		.stdout(process::Stdio::piped())
		.stderr(process::Stdio::piped())
		.spawn()
		.unwrap();
	let mut fabric_stdout = fabric.stdout.take().unwrap();
	let fabric_stdout = thread::spawn(move || {
		let mut none = true;
		loop {
			use std::io::Read;
			let mut stdout = vec![0; 1024];
			// println!("awaiting stdout");
			let n = fabric_stdout.read(&mut stdout).unwrap();
			// println!("got stdout {}", n);
			if n == 0 {
				break;
			}
			none = false;
			stdout.truncate(n);
			println!("fab stdout: {:?}", String::from_utf8(stdout).unwrap());
		}
		none
	});
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
	while TcpStream::connect(FABRIC_ADDR).is_err() {
		// TODO: parse output rather than this loop and timeout
		if start_.elapsed() > time::Duration::new(2, 0) {
			panic!("Fabric not up within 2s");
		}
		thread::sleep(std::time::Duration::new(0, 1_000_000));
	}
	let start_ = time::Instant::now();
	while TcpStream::connect(BRIDGE_ADDR).is_err() {
		// TODO: parse output rather than this loop and timeout
		if start_.elapsed() > time::Duration::new(10, 0) {
			panic!("Bridge not up within 10s");
		}
		thread::sleep(std::time::Duration::new(0, 1_000_000));
	}

	let mut products = products
		.iter()
		.filter(|&(src, _bin)| src.starts_with(Path::new(TESTS)) && src != Path::new(SELF))
		.collect::<Vec<_>>();
	products.sort_by(|&(ref a_src, _), &(ref b_src, _)| a_src.cmp(b_src));

	let (mut succeeded, mut failed) = (0, 0);
	for (src, bin) in products {
		// if src != Path::new("tests/s.rs") {
		// 	continue;
		// }
		// if src != Path::new("tests/x.rs") {
		// 	continue;
		// }
		println!("{}", src.display());
		let mut file: Result<OutputTest, _> = serde_json::from_str(
			&BufReader::new(File::open(src).unwrap())
				.lines()
				.map(Result::unwrap)
				.take_while(|x| x.get(0..3) == Some("//="))
				.flat_map(|x| ext::string::Chars::new(x).skip(3))
				.collect::<String>(),
		);
		if !FORWARD_STDERR {
			file = file.map(remove_stderr);
		}
		let mut x = |command: &mut process::Command| {
			let result = command.output().unwrap();
			let output = parse_output(&result);
			if output.is_err()
				|| file.is_err() || !file.as_ref().unwrap().is_match(output.as_ref().unwrap())
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
			x(process::Command::new(bin)
				.env_remove("CONSTELLATION_VERSION")
				.env("CONSTELLATION_FORMAT", "json"));
		}
		print!("{:?}", SystemLoad::measure());
		println!("  deployed");
		for i in 0..iterations {
			println!("    {}", i);
			x(process::Command::new(deploy)
				.env_remove("CONSTELLATION_VERSION")
				.env_remove("CONSTELLATION_FORMAT")
				.args(&["--format=json", BRIDGE_ADDR, bin.to_str().unwrap()]));
		}
		print!("{:?}", SystemLoad::measure());
	}

	println!("killing");
	fabric.kill().unwrap();
	let _ = fabric.wait().unwrap();
	let _stderr_empty = fabric_stderr.join().unwrap();
	// assert!(stderr_empty);
	let _stdout_empty = fabric_stdout.join().unwrap();
	// assert!(stdout_empty);

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

struct SystemLoad {
	memory: Result<systemstat::data::Memory, io::Error>,
	load_average: Result<systemstat::data::LoadAverage, io::Error>,
	load_aggregate: Result<systemstat::data::CPULoad, io::Error>,
	processes: usize,
	threads: usize,
	file_stats: Result<systemstat::data::FileStats, io::Error>,
}
impl SystemLoad {
	fn measure() -> Self {
		let sys = System::new();
		Self {
			memory: sys.memory(),
			load_average: sys.load_average(),
			load_aggregate: sys.cpu_load_aggregate().and_then(|cpu| {
				thread::sleep(Duration::from_millis(500)); // TODO: make sleep opt-in
				cpu.done()
			}),
			processes: palaver::process::count(),
			threads: palaver::process::count_threads(),
			file_stats: sys.file_stats(),
		}
	}
}
impl fmt::Debug for SystemLoad {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match &self.memory {
			Ok(mem) => writeln!(
				f,
				"Memory: {} used / {} ({} bytes) total ({:?})",
				saturating_sub_bytes(mem.total, mem.free),
				mem.total,
				mem.total.as_u64(),
				mem.platform_memory
			)?,
			Err(x) => writeln!(f, "Memory: error: {}", x)?,
		}
		match &self.load_average {
			Ok(loadavg) => writeln!(
				f,
				"Load average: {} {} {}",
				loadavg.one, loadavg.five, loadavg.fifteen
			)?,
			Err(x) => writeln!(f, "Load average: error: {}", x)?,
		}
		match &self.load_aggregate {
			Ok(cpu) => writeln!(
				f,
				"CPU load: {}% user, {}% nice, {}% system, {}% intr, {}% idle",
				cpu.user * 100.0,
				cpu.nice * 100.0,
				cpu.system * 100.0,
				cpu.interrupt * 100.0,
				cpu.idle * 100.0
			)?,
			Err(x) => writeln!(f, "CPU load: error: {}", x)?,
		}
		writeln!(
			f,
			"Processes: {}, threads: {}",
			self.processes, self.threads
		)?;
		if let Ok(file_stats) = &self.file_stats {
			writeln!(
				f,
				"File descriptors: {} open, {} max",
				file_stats.open, file_stats.max
			)?;
		}
		Ok(())
	}
}
