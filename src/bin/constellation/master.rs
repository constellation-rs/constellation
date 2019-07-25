use bincode;
use crossbeam;
use either::Either;
use palaver::file::copy;
use std::{
	collections::{HashMap, HashSet, VecDeque}, convert::{TryFrom, TryInto}, env, ffi::OsString, fs, io::{self, Read, Write}, net, path, sync::mpsc
};
use serde::Serialize;

use constellation_internal::{map_bincode_err, BufferedStream, Pid, PidInternal, Resources};

#[derive(Debug)]
pub struct Node {
	mem: u64,
	cpu: f32,
}
impl Node {
	fn fits(&self, process: &Resources) -> bool {
		process.mem <= self.mem && process.cpu <= self.cpu
	}

	fn alloc(&mut self, process: &Resources) {
		assert!(process.cpu <= self.cpu);
		self.mem -= process.mem;
		self.cpu -= process.cpu;
	}

	fn free(&mut self, process: &Resources) {
		self.mem += process.mem;
		self.cpu += process.cpu;
	}
}

#[derive(Serialize)]
struct SchedulerArg {
	scheduler: net::SocketAddr,
}

fn parse_request<R: Read>(
	mut stream: &mut R,
) -> Result<
	(
		Resources,
		Vec<OsString>,
		Vec<(OsString, OsString)>,
		Vec<u8>,
		Vec<u8>,
	),
	io::Error,
> {
	let process = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let args = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let vars = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let len: u64 = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let mut binary = Vec::with_capacity(len.try_into().unwrap());
	copy(stream, &mut binary, len)?;
	assert_eq!(binary.len(), usize::try_from(len).unwrap());
	let arg = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	Ok((process, args, vars, binary, arg))
}

pub fn run(
	addr: net::SocketAddr,
	nodes: HashMap<net::SocketAddr, (u64, f32, Vec<(path::PathBuf, Vec<net::SocketAddr>)>)>,
) {
	let (sender, receiver) = mpsc::sync_channel::<
		Either<
			(
				Resources,
				Vec<OsString>,
				Vec<(OsString, OsString)>,
				Vec<u8>,
				Vec<u8>,
				mpsc::SyncSender<Option<Pid>>,
				Option<usize>,
				Vec<net::SocketAddr>,
			),
			(usize, Either<u16, u16>),
		>,
	>(0);

	let mut nodes = nodes
		.into_iter()
		.enumerate()
		.map(|(i, (addr, (mem, cpu, bridges)))| {
			let node = Node { mem, cpu };
			let mut check_addresses = HashSet::new();
			let check_port = check_addresses.insert(addr);
			assert!(check_port);
			let (sender_a, receiver_a) = mpsc::sync_channel::<(
				Resources,
				Vec<OsString>,
				Vec<(OsString, OsString)>,
				Vec<u8>,
				Vec<u8>,
				Vec<net::SocketAddr>,
			)>(0);
			let stream = net::TcpStream::connect(&addr).unwrap();
			let local_addr = stream.local_addr().unwrap().ip();
			let sender1 = sender.clone();
			let _ = std::thread::Builder::new().spawn(move || {
				let (receiver, sender) = (receiver_a, sender1);
				let (mut stream_read, mut stream_write) =
					(BufferedStream::new(&stream), BufferedStream::new(&stream));
				crossbeam::scope(|scope| {
					let _ = scope.spawn(|| {
						for (process, args, vars, binary, arg, ports) in receiver {
							let mut stream_write = stream_write.write();
							bincode::serialize_into(&mut stream_write, &process).unwrap();
							bincode::serialize_into(&mut stream_write, &ports).unwrap(); // TODO: do all ports before everything else
							bincode::serialize_into(&mut stream_write, &args).unwrap();
							bincode::serialize_into(&mut stream_write, &vars).unwrap();
							bincode::serialize_into(&mut stream_write, &(binary.len() as u64))
								.unwrap();
							stream_write.write_all(&binary).unwrap();
							bincode::serialize_into(&mut stream_write, &arg).unwrap();
							drop(stream_write);
						}
					});
					let _ = scope.spawn(|| {
						let sender = sender;
						while let Ok(done) =
							bincode::deserialize_from::<_, Either<u16, u16>>(&mut stream_read)
								.map_err(map_bincode_err)
						{
							sender.send(Either::Right((i, done))).unwrap();
						}
					});
				});
			}).unwrap();
			for (bridge, ports) in bridges {
				for &port in &ports {
					let check_port = check_addresses.insert(port);
					assert!(check_port);
				}
				let sender = sender.clone();
				let _ = std::thread::Builder::new().spawn(move || {
					let mut path = env::current_exe().unwrap();
					let _ = path.pop();
					path.push(&bridge);
					let mut file_in = fs::File::open(&path)
						.or_else(|_| fs::File::open(&bridge))
						.unwrap_or_else(|_| panic!("Failed to open bridge {:?}", &bridge));
					let mut binary = Vec::new();
					let _ = file_in.read_to_end(&mut binary).unwrap();
					let (sender_, receiver) = mpsc::sync_channel::<Option<Pid>>(0);
					sender
						.send(Either::Left((
							Resources { mem: 0, cpu: 0.0 },
							vec![OsString::from(bridge)],
							Vec::new(),
							binary,
							Vec::new(),
							sender_,
							Some(i),
							ports,
						)))
						.unwrap();
					let pid: Option<Pid> = receiver.recv().unwrap();
					println!("bridge at {:?}", pid.unwrap());
				}).unwrap();
			}
			(sender_a, node, addr.ip(), local_addr, VecDeque::new())
		})
		.collect::<Vec<_>>();

	let listener = net::TcpListener::bind(addr).unwrap();
	let _ = std::thread::Builder::new().spawn(move || {
		for stream in listener.incoming() {
			// println!("accepted");
			let stream = stream.unwrap();
			let sender = sender.clone();
			let _ = std::thread::Builder::new().spawn(move || {
				let (mut stream_read, mut stream_write) = (BufferedStream::new(&stream), &stream);
				while let Ok((process, args, vars, binary, arg)) = parse_request(&mut stream_read) {
					// println!("parsed");
					let (sender_, receiver) = mpsc::sync_channel::<Option<Pid>>(0);
					sender
						.send(Either::Left((
							process,
							args,
							vars,
							binary,
							arg,
							sender_,
							None,
							vec![],
						)))
						.unwrap();
					let pid: Option<Pid> = receiver.recv().unwrap();
					// let mut stream_write = stream_write.write();
					if bincode::serialize_into(&mut stream_write, &pid).is_err() {
						break;
					}
				}
			}).unwrap();
		}
	}).unwrap();

	let mut processes: HashMap<(usize, u16), Resources> = HashMap::new();

	for msg in receiver.iter() {
		match msg {
			Either::Left((process, args, vars, binary, arg, sender, force, ports)) => {
				println!("spawn {:?}", process);
				let node = if force.is_none() {
					nodes.iter().position(|node| node.1.fits(&process))
				} else {
					Some(force.unwrap())
				};
				if let Some(node) = node {
					let node = &mut nodes[node];
					node.1.alloc(&process);

					let mut sched_arg = Vec::new();
					bincode::serialize_into(
						&mut sched_arg,
						&SchedulerArg {
							scheduler: net::SocketAddr::new(node.3, addr.port()),
						},
					)
					.unwrap();
					sched_arg.extend(arg);
					node.0
						.send((process, args, vars, binary, sched_arg, ports))
						.unwrap();
					node.4.push_back((sender, process));
				} else {
					println!(
						"Failing a spawn! Cannot allocate process {:#?} to nodes {:#?}",
						process, nodes
					);
					sender.send(None).unwrap();
				}
			}
			Either::Right((node_, Either::Left(init))) => {
				println!("init {}:{}", node_, init);
				let node = &mut nodes[node_];
				let (sender, process) = node.4.pop_front().unwrap();
				let x = processes.insert((node_, init), process);
				assert!(x.is_none());
				let pid = Pid::new(node.2, init);
				sender.send(Some(pid)).unwrap();
			}
			Either::Right((node, Either::Right(done))) => {
				let process = processes.remove(&(node, done)).unwrap();
				println!("done {}:{}", node, done);
				let node = &mut nodes[node];
				node.1.free(&process);
			}
		}
	}
}
