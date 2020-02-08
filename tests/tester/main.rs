//! This is the testsuite for `deploy`. It runs the tests in `test/src/bin/` first directly, and then deployed to a local `fabric`.
//!
//! At the top of each test is some JSON, denoted with the special comment syntax `//=`.
//! `output` is a hashmap of file descriptor to a regex of expected output. As it is a regex ensure that any literal `\.+*?()|[]{}^$#&-~` are escaped.

#![feature(backtrace)]
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
	clippy::derive_hash_xor_eq,
	clippy::filter_map
)]

mod ext;

use multiset::HashMultiSet;
use serde::{Deserialize, Serialize};
use std::{
	collections::HashMap, env, ffi::OsStr, fmt, fmt::Debug, fs::{self, File}, hash, io::{self, BufRead, BufReader}, iter, mem, net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream}, path::{Path, PathBuf}, process, str, thread, thread::JoinHandle, time::{self, Duration}
};
use systemstat::{saturating_sub_bytes, Platform, System};

use constellation_internal::{abort_on_unwind, Cpu, ExitStatus, Fd, Mem};
use ext::serialize_as_regex_string::SerializeAsRegexString;

const DEPLOY: &str = "src/bin/deploy.rs";
const FABRIC: &str = "src/bin/constellation/main.rs";
const TESTS: &str = "tests/";
const SELF: &str = "tests/tester/main.rs";

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

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

#[allow(clippy::too_many_lines)]
fn main() {
	let start = time::Instant::now();
	std::env::set_var("RUST_BACKTRACE", "full");
	std::panic::set_hook(Box::new(|info| {
		eprintln!(
			"thread '{}' {}",
			thread::current().name().unwrap_or("<unnamed>"),
			info
		);
		eprintln!("{:?}", std::backtrace::Backtrace::force_capture());
		std::process::abort();
	}));
	let _ = thread::Builder::new()
		.spawn(abort_on_unwind(move || loop {
			thread::sleep(time::Duration::new(10, 0));
			println!("{:?}", start.elapsed());
		}))
		.unwrap();
	let current_dir = env::current_dir().unwrap();
	let mut products = HashMap::new();
	let iterations: usize = env::var("CONSTELLATION_TEST_ITERATIONS")
		.map(|x| x.parse().unwrap())
		.unwrap_or(10);
	let tests = fs::read_dir(TESTS).unwrap().filter_map(|path| {
		let path = path.ok()?.path();
		if path.extension()? == "rs" {
			Some(String::from(path.file_stem()?.to_str()?))
		} else {
			None
		}
	});
	let target_dir = Path::new(env!("OUT_DIR"))
		.ancestors()
		.nth(4)
		.and_then(Path::file_name);
	debug_assert!(
		target_dir.unwrap() == "target" || target_dir.unwrap() == env!("TARGET"),
		"{:?}",
		target_dir
	);
	// This is to avoid building twice, once with unset and once with set --target
	let target = if target_dir != Some(OsStr::new("target")) {
		Some(format!("--target={}", env!("TARGET")))
	} else {
		None
	};
	let tests = tests.map(|test| format!("--test={}", test));
	let profile = match env!("PROFILE") {
		"debug" => None,
		"release" => Some(String::from("--release")),
		_ => unreachable!(),
	};
	let features = format!("--features={}", env!("FEATURES"));
	let args = iter::once(String::from("build"))
		.chain(tests)
		.chain(iter::once(String::from("--message-format=json")))
		.chain(target)
		.chain(iter::once(String::from("--no-default-features")))
		.chain(iter::once(features))
		.chain(profile)
		.collect::<Vec<_>>();
	println!(
		"Building with: cargo {}",
		args.iter()
			.map(|arg| if arg.contains(' ') {
				format!("\"{}\"", arg)
			} else {
				arg.to_owned()
			})
			.collect::<Vec<_>>()
			.join(" ")
	);
	let output = process::Command::new("cargo")
		.args(&args)
		.stderr(process::Stdio::inherit())
		.output()
		.expect("Failed to invoke cargo");
	if !output.status.success() {
		panic!("cargo build failed");
	}
	for message in serde_json::Deserializer::from_slice(&output.stdout).into_iter() {
		if let cargo_metadata::Message::CompilerArtifact(artifact) = message.unwrap_or_else(|_| {
			panic!(
				"Failed to parse output of cargo {}",
				itertools::join(args.iter(), " ")
			)
		}) {
			if let Ok(path) = PathBuf::from(&artifact.target.src_path).strip_prefix(&current_dir) {
				if (artifact.target.kind == vec![String::from("bin")] && !artifact.profile.test)
					|| artifact.target.kind == vec![String::from("test")]
				{
					let x = products.insert(
						path.to_owned(),
						artifact.filenames.into_iter().next().unwrap(),
					);
					// We're assuming the first filename is the binary – .dSYM etc seem to always be second?
					assert!(x.is_none());
				}
			}
		}
	}

	let (deploy, fabric) = (&products[Path::new(DEPLOY)], &products[Path::new(FABRIC)]);
	let mut products = products
		.iter()
		.filter(|&(src, _bin)| src.starts_with(Path::new(TESTS)) && src != Path::new(SELF))
		.map(|(src, bin)| {
			let mut file: Result<OutputTest, serde_json::Error> = serde_json::from_str(
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
			(src, bin, file)
		})
		.collect::<Vec<_>>();
	products.sort_by(|&(ref a_src, _, _), &(ref b_src, _, _)| a_src.cmp(b_src));

	let node1_fabric_port = 12340;
	let node1_bridge_port = 12341;
	let node2_fabric_port = 12350;
	let node3_fabric_port = 12360;

	let (mut succeeded, mut failed) = (0, 0);
	for environment in &mut [
		&mut Native as &mut dyn Environment,
		&mut Cluster::new(
			deploy.clone(),
			fabric.clone(),
			vec![
				Node {
					fabric: Socket::localhost(node1_fabric_port),
					bridge: Some(Socket::localhost(node1_bridge_port)),
					mem: 2 * Mem::GIB,
					cpu: 2 * Cpu::CORE,
				},
				Node {
					fabric: Socket::localhost(node2_fabric_port),
					bridge: None,
					mem: Mem::GIB,
					cpu: Cpu::CORE,
				},
				Node {
					fabric: Socket::localhost(node3_fabric_port),
					bridge: None,
					mem: Mem::GIB / 2,
					cpu: Cpu::CORE / 2,
				},
			],
		),
	] {
		println!("Running in environment: {:#?}", environment);
		environment.start();
		for (ref src, ref bin, ref file) in &products {
			println!("{}", src.display());
			for i in 0..iterations {
				println!("    {}", i);
				let result = environment.run(bin);
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
			}
			print!("{:?}", SystemLoad::measure());
		}
		environment.stop();
	}

	println!(
		"{}/{} succeeded in {:?}",
		succeeded,
		succeeded + failed,
		start.elapsed()
	);
	if failed > 0 || succeeded == 0 {
		process::exit(1);
	}
}

trait Environment: Debug {
	fn start(&mut self) {}
	fn run(&mut self, bin: &Path) -> process::Output;
	fn stop(&mut self) {}
}

#[derive(Debug)]
struct Native;
impl Environment for Native {
	fn run(&mut self, bin: &Path) -> process::Output {
		process::Command::new(bin)
			.env_remove("CONSTELLATION_VERSION")
			.env("CONSTELLATION_FORMAT", "json")
			.output()
			.unwrap()
	}
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum Cluster {
	Init(ClusterConfig),
	Running {
		config: ClusterConfig,
		cluster: Vec<ClusterNode>,
	},
	Invalid,
}
#[derive(Debug)]
struct ClusterConfig {
	deploy: PathBuf,
	fabric: PathBuf,
	nodes: Vec<Node>,
}
#[derive(Debug)]
struct Node {
	fabric: Socket,
	bridge: Option<Socket>,
	mem: Mem,
	cpu: Cpu,
}
#[derive(Clone, Copy, Debug)]
struct Socket {
	bind: SocketAddr,
	external: SocketAddr,
}
impl Socket {
	fn localhost(port: u16) -> Self {
		let bind = SocketAddr::new(LOCALHOST, port);
		Self {
			bind,
			external: bind,
		}
	}
}
#[derive(Debug)]
struct ClusterNode {
	process: process::Child,
	stdout: JoinHandle<bool>,
	stderr: JoinHandle<bool>,
}

impl Cluster {
	fn new(deploy: PathBuf, fabric: PathBuf, nodes: Vec<Node>) -> Self {
		Cluster::Init(ClusterConfig {
			deploy,
			fabric,
			nodes,
		})
	}
}
impl Environment for Cluster {
	fn start(&mut self) {
		let config = if let Self::Init(config) = mem::replace(self, Cluster::Invalid) {
			config
		} else {
			panic!()
		};
		let mut cluster = config
			.nodes
			.iter()
			.enumerate()
			.rev()
			.map(|(i, node)| {
				if TcpStream::connect(node.fabric.external).is_ok() {
					panic!(
						"Service already running on FABRIC_ADDR {}",
						node.fabric.external
					);
				}
				if let Some(bridge) = node.bridge {
					if TcpStream::connect(bridge.external).is_ok() {
						panic!("Service already running on BRIDGE_ADDR {}", bridge.external);
					}
				}
				let mut args = vec![
					String::from("--format"),
					String::from("json"),
					String::from("-v"),
					node.fabric.bind.to_string(),
				];
				if i == 0 {
					for node in &config.nodes {
						args.extend(vec![
							node.fabric.external.to_string(),
							node.bridge.map_or_else(
								|| String::from("-"),
								|bridge| bridge.bind.to_string(),
							),
							node.mem.to_string(),
							node.cpu.to_string(),
						]);
					}
				}
				let mut process = process::Command::new(&config.fabric)
					.args(args)
					.stdin(process::Stdio::null())
					.stdout(process::Stdio::piped())
					.stderr(process::Stdio::piped())
					.spawn()
					.unwrap();
				let stdout = process.stdout.take().unwrap();
				let stdout = capture_stdout(stdout, true, move |none, output| {
					*none = false;
					println!("fab stdout: {:?}", str::from_utf8(output).unwrap());
				});
				let stderr = process.stderr.take().unwrap();
				let stderr = capture_stdout(stderr, true, move |none, output| {
					*none = false;
					println!("fab stderr: {:?}", str::from_utf8(output).unwrap());
				});
				let start_ = time::Instant::now();
				while TcpStream::connect(node.fabric.external).is_err() {
					// TODO: parse output rather than this loop and timeout
					if start_.elapsed() > time::Duration::new(5, 0) {
						panic!("Fabric not up within 5s");
					}
					thread::sleep(std::time::Duration::new(0, 1_000_000));
				}
				if let Some(bridge) = node.bridge {
					while TcpStream::connect(bridge.external).is_err() {
						// TODO: parse output rather than this loop and timeout
						if start_.elapsed() > time::Duration::new(15, 0) {
							panic!("Bridge not up within 15s");
						}
						thread::sleep(std::time::Duration::new(0, 1_000_000));
					}
				}
				ClusterNode {
					process,
					stdout,
					stderr,
				}
			})
			.collect::<Vec<_>>();
		cluster.reverse();
		*self = Cluster::Running { config, cluster }
	}
	fn run(&mut self, bin: &Path) -> process::Output {
		let config = if let Self::Running { config, .. } = self {
			config
		} else {
			panic!()
		};
		process::Command::new(&config.deploy)
			.env_remove("CONSTELLATION_VERSION")
			.env_remove("CONSTELLATION_FORMAT")
			.args(&[
				"--format=json",
				&config.nodes[0].bridge.unwrap().external.to_string(),
				bin.to_str().unwrap(),
			])
			.output()
			.unwrap()
	}
	fn stop(&mut self) {
		let (config, cluster) =
			if let Self::Running { config, cluster } = mem::replace(self, Self::Invalid) {
				(config, cluster)
			} else {
				panic!()
			};
		for mut node in cluster {
			println!("killing");
			node.process.kill().unwrap();
			println!("waiting");
			let _ = node.process.wait().unwrap();
			println!("waiting stderr");
			let stderr_empty = node.stderr.join().unwrap();
			assert!(stderr_empty);
			println!("waiting stdout");
			let _stdout_empty = node.stdout.join().unwrap();
			// assert!(stdout_empty);
		}
		*self = Self::Init(config);
	}
}

fn capture_stdout<
	R: io::Read + Send + 'static,
	F: FnMut(&mut S, &[u8]) + Send + 'static,
	S: Send + 'static,
>(
	mut read: R, mut state: S, mut f: F,
) -> JoinHandle<S> {
	thread::spawn(abort_on_unwind(move || {
		let mut buf = vec![0; 16 * 1024];
		loop {
			let n = read.read(&mut buf).unwrap();
			if n == 0 {
				break state;
			}
			f(&mut state, &buf[..n]);
		}
	}))
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
impl Debug for SystemLoad {
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
