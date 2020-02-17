#![allow(clippy::too_many_lines)]

use serde::Deserialize;
use std::{error::Error, fs, net::SocketAddr};

use super::{Args, MeshRole, Node, Role};
use constellation_internal::{Cpu, Format, Key, Mem};

const DESCRIPTION: &str = r"Run a constellation node.
";
const USAGE: &str = r"USAGE:
    constellation <bind> [<nodes.toml>]

OPTIONS:
    -h --help           Show this screen.
    -V --version        Show version.
    -v --verbose        Verbose output.
       --format [human|json]
                        Output format [default: human].
";
const HELP: &str = r#"A constellation cluster consists of one or more nodes. A node is started like
this:

    constellation 10.0.0.2:9999

This binds to 10.0.0.2:9999, listening for connections from other nodes.

In order for nodes to know about each other, one node must be started with a
list of all the nodes:

    constellation 10.0.0.1:9999 nodes.toml

For more information, see the documentation at
https://github.com/alecmocatta/constellation/ or run again with --help -v
"#;
const VERBOSE_HELP: &str = r#"A constellation cluster consists of one or more nodes. A node is started like
this:

    constellation 10.0.0.2:9999

This binds to 10.0.0.2:9999, listening for connections from other nodes.

In order for nodes to know about each other, one node must be started with a
list of all the nodes:

    constellation 10.0.0.1:9999 nodes.toml

This reads the file "nodes.toml", which should look something like this:

    [[nodes]]
    fabric_addr = "10.0.0.1:9999"
    bridge_bind = "10.0.0.1:8888"
    mem = "5 GiB"
    cpu = 1

    [[nodes]]
    fabric_addr = "10.0.0.2:9999"
    mem = "5 GiB"
    cpu = 1

This enables the nodes to see and communicate with each other.

Deploying to this cluster might then be:

    deploy 10.0.0.1:8888 ./binary

or, for a Rust crate:

    cargo deploy 10.0.0.1:8888
"#;
//     constellation master <bind> (<fabric_addr> [<bridge_bind>] <mem> <cpu>)...
// The arguments to the master node are the address and resources of each node,
// including itself. The arguments for a node can include a binary to spawn
// immediately and an address to reserve for it. This is intended to be used to
// spawn the bridge, which works with the deploy command and library to handle
// transparent capture and forwarding of output and debug information.

// The first set of arguments to the master node is for itself – as such the
// address is bound to not connected to.

// The argument to non-master nodes is the address to bind to.

// For example, respective invocations on each of a cluster of 3 servers with
// 512GiB memory and 36 logical cores apiece might be:
//     constellation master 10.0.0.1 \
//                   10.0.0.1:9999 10.0.0.1:8888 400GiB 34 \
//                   10.0.0.2:9999 10.0.0.2:8888 400GiB 34 \
//                   10.0.0.3:9999 10.0.0.3:8888 400GiB 34
//     constellation 10.0.0.2:9999
//     constellation 10.0.0.3:9999

impl Args {
	pub fn from_args(args: impl Iterator<Item = String>) -> Result<Self, (String, bool)> {
		let mut args = args.peekable();
		let mut format = None;
		let mut verbose = false;
		let mut help = None;
		loop {
			match args.peek().map(|x| &**x) {
				arg @ None | arg @ Some("-h") | arg @ Some("--help") if help.is_none() => {
					help = Some(arg.is_some());
					let _ = args.next();
				}
				Some("-V") | Some("--version") => {
					return Err((format!("constellation {}", env!("CARGO_PKG_VERSION")), true))
				}
				Some("-v") | Some("--verbose") if !verbose => {
					let _ = args.next().unwrap();
					verbose = true
				}
				Some("--format") if format.is_none() => {
					let _ = args.next().unwrap();
					match args.next().as_ref().map(|x| &**x) {
						Some("json") => format = Some(Format::Json),
						Some("human") => format = Some(Format::Human),
						_ => {
							return Err((
								format!(
									"Invalid format, expecting \"json\" or \"human\"\n{}",
									USAGE
								),
								false,
							));
						}
					}
				}
				Some(format_) if format.is_none() && format_.starts_with("--format=") => {
					let format_ = args.next().unwrap();
					match &format_[9..] {
						"json" => format = Some(Format::Json),
						"human" => format = Some(Format::Human),
						_ => {
							return Err((
								format!(
									"Invalid format, expecting \"json\" or \"human\"\n{}",
									USAGE
								),
								false,
							));
						}
					}
				}
				_ => break,
			}
		}
		if let Some(success) = help {
			return Err((
				if verbose {
					format!("{}\n{}\n{}", DESCRIPTION, USAGE, VERBOSE_HELP)
				} else {
					format!("{}\n{}\n{}", DESCRIPTION, USAGE, HELP)
				},
				success,
			));
		}
		let format = format.unwrap_or(Format::Human);
		let arg = args.next().unwrap();
		let role: Role = match &*arg {
			"bridge" => Role::Bridge,
			#[cfg(feature = "kubernetes")]
			"kube" => {
				if let (
					Some(Ok(master_bind)),
					Some(Ok(bridge_bind)),
					Some(Ok(mem)),
					Some(Ok(cpu)),
					Some(Ok(replicas)),
					None,
				) = (
					args.next().map(|x| x.parse::<SocketAddr>()),
					args.next().map(|x| x.parse::<SocketAddr>()),
					args.next().map(|x| x.parse::<Mem>()),
					args.next().map(|x| x.parse::<Cpu>()),
					args.next().map(|x| x.parse::<u32>()),
					args.next(),
				) {
					Role::KubeMaster {
						master_bind,
						bridge_bind,
						mem,
						cpu,
						replicas,
					}
				} else {
					return Err((format!("Invalid kubernetes master options, expecting <addr> <addr> <mem> <cpu>, like 127.0.0.1:9999 127.0.0.1:8888 400GiB 34\n{}", USAGE), false));
				}
			}
			"master" | "mesh-master" => {
				let mesh = arg == "mesh-master";
				let err = || {
					(format!("Invalid args, expecting master <bind> <key> <nodes>, like master 127.0.0.1:9999 - 127.0.0.1:8888 - 400GiB 34\n{}", USAGE), false)
				};
				let bind = args.next().ok_or_else(err)?.parse().map_err(|_| err())?;
				let key = args.next().ok_or_else(err)?.parse().map_err(|_| err())?;
				let mut nodes = Vec::new();
				let mut arg: Option<String> = Some(args.next().ok_or_else(err)?);
				loop {
					match (
						arg.take().or_else(|| args.next()).map(|x| x.parse()),
						args.next().map(|x| {
							if x == "-" {
								Ok(None)
							} else {
								x.parse().map(Some)
							}
						}),
						args.next().map(|x| x.parse::<Mem>()),
						args.next().map(|x| x.parse::<Cpu>()),
					) {
						(None, _, _, _) if !nodes.is_empty() => break,
						(Some(Ok(fabric)), Some(Ok(bridge)), Some(Ok(mem)), Some(Ok(cpu))) => {
							nodes.push(Node {
								fabric,
								bridge,
								mem,
								cpu,
							});
						}
						_ => {
							return Err((format!("Invalid node options, expecting <addr> <addr> <mem> <cpu>, like 127.0.0.1:9999 127.0.0.1:8888 400GiB 34\n{}", USAGE), false));
						}
					}
				}
				if nodes.is_empty() {
					return Err((format!("At least one node must be present: expecting <addr> <addr> <mem> <cpu>, like 127.0.0.1:9999 127.0.0.1:8888 400GiB 34\n{}", USAGE), false));
				}
				if mesh {
					Role::Mesh(MeshRole::Master(bind, key, nodes))
				} else {
					Role::Master(bind, key, nodes)
				}
			}
			"mesh-worker" => {
				let bind = args.next().unwrap().parse().unwrap();
				let key = args.next().unwrap().parse().unwrap();
				Role::Mesh(MeshRole::Worker(bind, key))
			}
			bind if bind.parse::<SocketAddr>().is_ok() => {
				let bind = bind.parse().unwrap();
				let key = match args.peek() {
					Some(key) if key.parse::<Key>().is_ok() => {
						Some(args.next().unwrap().parse().unwrap())
					}
					_ => None,
				}
				.unwrap_or_default();
				if let Some(toml) = args.next() {
					let err = |e| {
						(
							format!("Can't open the TOML file \"{}\": {}", toml, e),
							false,
						)
					};
					let nodes = Self::from_toml(&toml).map_err(err)?;
					Role::Master(bind, key, nodes)
				} else {
					if let Some(x) = args.next() {
						return Err((format!("Invalid arg \"{}\", expecting an address to bind to, like 127.0.0.1:9999\n{}", x, USAGE), false));
					}
					Role::Worker(bind, key)
				}
			}
			x => {
				return Err((format!("Invalid option \"{}\", expecting an address to bind to, like 127.0.0.1:9999\n{}", x, USAGE), false));
			}
		};
		Ok(Self {
			format,
			verbose,
			role,
		})
	}
	fn from_toml(path: &str) -> Result<Vec<Node>, Box<dyn Error>> {
		#[derive(Deserialize)]
		struct A {
			nodes: Vec<B>,
		}
		#[derive(Deserialize)]
		struct B {
			fabric_addr: SocketAddr,
			bridge_bind: Option<SocketAddr>,
			mem: Mem,
			cpu: Cpu,
		}
		let toml = fs::read_to_string(path).map_err(|e| Box::new(e) as Box<dyn Error>)?;
		let nodes = toml::from_str::<A>(&*toml).map_err(|e| {
			format!(
				r#"{}
It should look something like:

[[nodes]]
fabric_addr = "10.0.0.1:9999"
bridge_bind = "10.0.0.1:8888"
mem = "5 GiB"
cpu = 1

[[nodes]]
fabric_addr = "10.0.0.2:9999"
mem = "5 GiB"
cpu = 1
"#,
				e
			)
		})?;
		if nodes.nodes.is_empty() {
			return Err("must contain multiple nodes".into());
		}
		Ok(nodes
			.nodes
			.into_iter()
			.map(|node| Node {
				fabric: node.fabric_addr,
				bridge: node.bridge_bind,
				mem: node.mem,
				cpu: node.cpu,
			})
			.collect())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn arg_parsing() {
		fn from_args(args: &[&'static str]) -> Result<Args, (String, bool)> {
			Args::from_args(args.iter().map(|arg| String::from(*arg)))
		}
		assert_eq!(
			from_args(&[]),
			Err((format!("{}\n{}\n{}", DESCRIPTION, USAGE, HELP), false))
		);
		assert_eq!(
			from_args(&["--format=json"]),
			Err((format!("{}\n{}\n{}", DESCRIPTION, USAGE, HELP), false))
		);
		assert_eq!(
			from_args(&["--format", "json"]),
			Err((format!("{}\n{}\n{}", DESCRIPTION, USAGE, HELP), false))
		);
		assert_eq!(
			from_args(&["-h"]),
			Err((format!("{}\n{}\n{}", DESCRIPTION, USAGE, HELP), true))
		);
		assert_eq!(
			from_args(&["10.0.0.1:8888"]),
			Ok(Args {
				format: Format::Human,
				verbose: false,
				role: Role::Worker("10.0.0.1:8888".parse().unwrap(), Key::default())
			})
		);
		assert_eq!(
			from_args(&["--format", "json", "10.0.0.1:8888"]),
			Ok(Args {
				format: Format::Json,
				verbose: false,
				role: Role::Worker("10.0.0.1:8888".parse().unwrap(), Key::default())
			})
		);
		assert_eq!(
			from_args(&["--format=json", "10.0.0.1:8888"]),
			Ok(Args {
				format: Format::Json,
				verbose: false,
				role: Role::Worker("10.0.0.1:8888".parse().unwrap(), Key::default())
			})
		);
		assert_eq!(
			from_args(&[
				"--format=json",
				"master",
				"10.0.0.1:8888",
				"-",
				"10.0.0.1:8888",
				"10.0.0.1:7777",
				"400GiB",
				"34",
			]),
			Ok(Args {
				format: Format::Json,
				verbose: false,
				role: Role::Master(
					"10.0.0.1:8888".parse().unwrap(),
					Key::default(),
					vec![Node {
						fabric: "10.0.0.1:8888".parse().unwrap(),
						bridge: Some("10.0.0.1:7777".parse().unwrap()),
						mem: 400 * Mem::GIB,
						cpu: 34 * Cpu::CORE,
					}]
				)
			})
		);
		assert_eq!(
			from_args(&[
				"--format=json",
				"master",
				"10.0.0.1:8888",
				"-",
				"10.0.0.1:8888",
				"10.0.0.1:7777",
				"400GiB",
				"34",
				"10.0.0.1:8888",
				"-",
				"400GiB",
				"34",
			]),
			Ok(Args {
				format: Format::Json,
				verbose: false,
				role: Role::Master(
					"10.0.0.1:8888".parse().unwrap(),
					Key::default(),
					vec![
						Node {
							fabric: "10.0.0.1:8888".parse().unwrap(),
							bridge: Some("10.0.0.1:7777".parse().unwrap()),
							mem: 400 * Mem::GIB,
							cpu: 34 * Cpu::CORE,
						},
						Node {
							fabric: "10.0.0.1:8888".parse().unwrap(),
							bridge: None,
							mem: 400 * Mem::GIB,
							cpu: 34 * Cpu::CORE,
						}
					]
				)
			})
		);
	}
}
