use std::{net::SocketAddr, path::PathBuf};

use super::{Args, Node, Role, Run};
use constellation_internal::{parse_binary_size, Format};

const DESCRIPTION: &str = r"
constellation
Run a constellation node, optionally as master, and optionally start a bridge running
";
const USAGE: &str = r"
USAGE:
    constellation master <addr> (<addr> <mem> <cpu> [<bridge> <addr>]...)...
    constellation <addr>

OPTIONS:
    -h --help          Show this screen.
    -V --version       Show version.
    -v --verbose       Verbose output.
    --format=<fmt>     Output format [possible values: human, json] [default: human]
";
const HELP: &str = r"
A constellation cluster comprises one or more nodes, where one is declared master.

The arguments to the master node are the address and resources of each node,
including itself. The arguments for a node can include a binary to spawn
immediately and an address to reserve for it. This is intended to be used to
spawn the bridge, which works with the deploy command and library to handle
transparent capture and forwarding of output and debug information.

The first set of arguments to the master node is for itself – as such the
address is bound to not connected to.

The argument to non-master nodes is the address to bind to.

For example, respective invocations on each of a cluster of 3 servers with
512GiB memory and 36 logical cores apiece might be:
    constellation master 10.0.0.1:9999 400GiB 34 bridge 10.0.0.1:8888 \
                  10.0.0.2:9999 400GiB 34 \
                  10.0.0.3:9999 400GiB 34
    constellation 10.0.0.2:9999
    constellation 10.0.0.3:9999

Deploying to this cluster might then be:
    deploy 10.0.0.1:8888 ./binary
or, for a Rust crate:
    cargo deploy 10.0.0.1:8888
";

impl Args {
	pub fn from_args(args: impl Iterator<Item = String>) -> Result<Self, (String, bool)> {
		let mut args = args.peekable();
		let mut format = None;
		let mut verbose = false;
		loop {
			match args.peek().map(|x| &**x) {
				arg @ None | arg @ Some("-h") | arg @ Some("--help") => {
					return Err((format!("{}{}{}", DESCRIPTION, USAGE, HELP), arg.is_some()));
				}
				Some("-V") | Some("--version") => {
					return Err((format!("constellation {}", env!("CARGO_PKG_VERSION")), true))
				}
				Some("-v") | Some("--verbose") => {
					let _ = args.next().unwrap();
					verbose = true
				}
				Some("--format") => {
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
				Some(format_) if format_.starts_with("--format=") => {
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
		let format = format.unwrap_or(Format::Human);
		let role: Role = match &*args.next().unwrap() {
			"master" => {
				let bind = match args.next().and_then(|x| x.parse().ok()) {
					Some(bind) => bind,
					None => return Err((format!("Invalid bind address\n{}", USAGE), false)),
				};
				let mut nodes = Vec::new();
				loop {
					match (
						args.next().map(|x| x.parse()),
						args.next().map(|x| parse_binary_size(&x)),
						args.next().map(|x| x.parse::<u32>()), // TODO
					) {
						(None, _, _) if !nodes.is_empty() => break,
						(Some(Ok(addr)), Some(Ok(mem)), Some(Ok(cpu))) => {
							let cpu = cpu * 65536; // TODO
							let mut run = Vec::new();
							while let Some(Err(_binary)) =
								args.peek().map(|x| x.parse::<SocketAddr>())
							{
								if let (binary, Some(Ok(addr))) =
									(args.next().unwrap(), args.next().map(|x| x.parse()))
								{
									run.push(Run {
										binary: PathBuf::from(binary),
										addr,
									});
								} else {
									return Err((format!("Invalid bridge, expecting bridge: <bridge> <addr> or node options: <addr> <mem> <cpu>\n{}", USAGE),false));
								}
							}
							nodes.push(Node {
								addr,
								mem,
								cpu,
								run,
							});
						}
						_ => {
							return Err((format!("Invalid node options, expecting <addr> <mem> <cpu>, like 127.0.0.1:9999 400GiB 34\n{}", USAGE),false));
						}
					}
				}
				Role::Master(bind, nodes)
			}
			x if x.parse::<SocketAddr>().is_ok() => Role::Worker(x.parse::<SocketAddr>().unwrap()),
			x => {
				return Err((format!("Invalid option \"{}\", expecting either \"master\" or an address, like 127.0.0.1:9999\n{}", x, USAGE),false));
			}
		};
		Ok(Self {
			format,
			verbose,
			role,
		})
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
			Err((format!("{}{}{}", DESCRIPTION, USAGE, HELP), false))
		);
		assert_eq!(
			from_args(&["--format=json"]),
			Err((format!("{}{}{}", DESCRIPTION, USAGE, HELP), false))
		);
		assert_eq!(
			from_args(&["--format", "json"]),
			Err((format!("{}{}{}", DESCRIPTION, USAGE, HELP), false))
		);
		assert_eq!(
			from_args(&["-h"]),
			Err((format!("{}{}{}", DESCRIPTION, USAGE, HELP), true))
		);
		assert_eq!(
			from_args(&["10.0.0.1:8888"]),
			Ok(Args {
				format: Format::Human,
				verbose: false,
				role: Role::Worker("10.0.0.1:8888".parse().unwrap())
			})
		);
		assert_eq!(
			from_args(&["--format", "json", "10.0.0.1:8888"]),
			Ok(Args {
				format: Format::Json,
				verbose: false,
				role: Role::Worker("10.0.0.1:8888".parse().unwrap())
			})
		);
		assert_eq!(
			from_args(&["--format=json", "10.0.0.1:8888"]),
			Ok(Args {
				format: Format::Json,
				verbose: false,
				role: Role::Worker("10.0.0.1:8888".parse().unwrap())
			})
		);
		assert_eq!(
			from_args(&[
				"--format=json",
				"master",
				"10.0.0.1",
				"10.0.0.1:8888",
				"400GiB",
				"34",
				"bridge",
				"10.0.0.1:7777"
			]),
			Ok(Args {
				format: Format::Json,
				verbose: false,
				role: Role::Master(
					"10.0.0.1".parse().unwrap(),
					vec![Node {
						addr: "10.0.0.1:8888".parse().unwrap(),
						mem: 400 * 1024 * 1024 * 1024,
						cpu: 34 * 65536,
						run: vec![Run {
							binary: PathBuf::from("bridge"),
							addr: "10.0.0.1:7777".parse().unwrap()
						}]
					}]
				)
			})
		);
	}
}
