use bincode;
use crossbeam;
use either::Either;
use serde::Serialize;
use std::{
	collections::{HashMap, HashSet, VecDeque}, env, ffi::OsString, fs, io::Read, net::{self, IpAddr}, path, sync::mpsc, thread
};

use constellation_internal::{map_bincode_err, msg::FabricRequest, BufferedStream, Pid, Resources};

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
	ip: IpAddr,
	scheduler: Pid,
}

pub fn run(
	bind_addr: net::SocketAddr, master_pid: Pid,
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
			(usize, Either<Pid, Pid>),
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
			let sender1 = sender.clone();
			let _ = thread::Builder::new()
				.spawn(move || {
					let (receiver, sender) = (receiver_a, sender1);
					let (mut stream_read, mut stream_write) =
						(BufferedStream::new(&stream), BufferedStream::new(&stream));
					bincode::serialize_into::<_, IpAddr>(&mut stream_write, &addr.ip()).unwrap();
					crossbeam::scope(|scope| {
						let _ = scope.spawn(|_spawn| {
							for (process, args, vars, binary, arg, ports) in receiver {
								let mut stream_write = stream_write.write();
								bincode::serialize_into(
									&mut stream_write,
									&FabricRequest::<Vec<u8>, Vec<u8>> {
										resources: process,
										bind: ports,
										args,
										vars,
										arg,
										binary,
									},
								)
								.unwrap();
								// bincode::serialize_into(&mut stream_write, &process).unwrap();
								// bincode::serialize_into(&mut stream_write, &ports).unwrap(); // TODO: do all ports before everything else
								// bincode::serialize_into(&mut stream_write, &args).unwrap();
								// bincode::serialize_into(&mut stream_write, &vars).unwrap();
								// bincode::serialize_into(&mut stream_write, &arg).unwrap();
								// bincode::serialize_into(&mut stream_write, &(binary.len() as u64))
								// 	.unwrap();
								// stream_write.write_all(&binary).unwrap();
								drop(stream_write);
							}
						});
						while let Ok(done) =
							bincode::deserialize_from::<_, Either<Pid, Pid>>(&mut stream_read)
								.map_err(map_bincode_err)
						{
							sender.send(Either::Right((i, done))).unwrap();
						}
					})
					.unwrap();
				})
				.unwrap();
			for (bridge, ports) in bridges {
				for &port in &ports {
					let check_port = check_addresses.insert(port);
					assert!(check_port);
				}
				let sender = sender.clone();
				let _ = thread::Builder::new()
					.spawn(move || {
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
						let _pid: Option<Pid> = receiver.recv().unwrap();
						// println!("bridge at {:?}", pid.unwrap());
					})
					.unwrap();
			}
			(sender_a, node, addr.ip(), VecDeque::new())
		})
		.collect::<Vec<_>>();

	let listener = net::TcpListener::bind(bind_addr).unwrap();
	let _ = thread::Builder::new()
		.spawn(move || {
			for stream in listener.incoming() {
				// println!("accepted");
				let stream = stream.unwrap();
				let sender = sender.clone();
				let _ = thread::Builder::new()
					.spawn(move || {
						let (mut stream_read, mut stream_write) =
							(BufferedStream::new(&stream), &stream);
						while let Ok((process, args, vars, binary, arg)) =
							bincode::deserialize_from(&mut stream_read).map_err(map_bincode_err)
						{
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
					})
					.unwrap();
			}
		})
		.unwrap();

	let mut processes: HashMap<(usize, Pid), Resources> = HashMap::new();

	for msg in receiver.iter() {
		match msg {
			Either::Left((process, args, vars, binary, arg, sender, force, ports)) => {
				// println!("spawn {:?}", process);
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
							ip: node.2,
							scheduler: master_pid,
						},
					)
					.unwrap();
					sched_arg.extend(arg);
					node.0
						.send((process, args, vars, binary, sched_arg, ports))
						.unwrap();
					node.3.push_back((sender, process));
				} else {
					// println!(
					// 	"Failing a spawn! Cannot allocate process {:#?} to nodes {:#?}",
					// 	process, nodes
					// );
					sender.send(None).unwrap();
				}
			}
			Either::Right((node_, Either::Left(pid))) => {
				// println!("init {}:{} ({})", node_, pid, processes.len());
				let node = &mut nodes[node_];
				let (sender, process) = node.3.pop_front().unwrap();
				let x = processes.insert((node_, pid), process);
				assert!(x.is_none());
				sender.send(Some(pid)).unwrap();
			}
			Either::Right((node, Either::Right(pid))) => {
				let process = processes.remove(&(node, pid)).unwrap();
				// println!("done {}:{} ({})", node, pid, processes.len());
				let node = &mut nodes[node];
				node.1.free(&process);
			}
		}
	}
}
