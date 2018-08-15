use bincode;
use crossbeam;
use deploy_common::{copy, map_bincode_err, BufferedStream, Pid, PidInternal, Resources};
use either::Either;
use std::{
	collections::{HashMap, HashSet, VecDeque}, env, ffi, fs, io::{self, Read, Write}, mem, net, path, sync::mpsc, thread
};

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
		Vec<ffi::OsString>,
		Vec<(ffi::OsString, ffi::OsString)>,
		Vec<u8>,
		Vec<u8>,
	),
	io::Error,
> {
	let process = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let args = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let vars = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let len: u64 = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	let mut elf = Vec::with_capacity(len as usize);
	copy(stream, &mut elf, len as usize)?;
	assert_eq!(elf.len(), len as usize);
	let arg = bincode::deserialize_from(&mut stream).map_err(map_bincode_err)?;
	Ok((process, args, vars, elf, arg))
}

pub fn run(
	addr: net::SocketAddr,
	nodes: HashMap<net::SocketAddr, (u64, f32, Vec<(path::PathBuf, Vec<net::SocketAddr>)>)>,
) {
	let (sender, receiver) = mpsc::sync_channel::<
		Either<
			(
				Resources,
				Vec<ffi::OsString>,
				Vec<(ffi::OsString, ffi::OsString)>,
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
				Vec<ffi::OsString>,
				Vec<(ffi::OsString, ffi::OsString)>,
				Vec<u8>,
				Vec<u8>,
				Vec<net::SocketAddr>,
			)>(0);
			let stream = net::TcpStream::connect(&addr).unwrap();
			let local_addr = stream.local_addr().unwrap().ip();
			let sender1 = sender.clone();
			thread::spawn(move || {
				let (receiver, sender) = (receiver_a, sender1);
				let (mut stream_read, mut stream_write) =
					(BufferedStream::new(&stream), BufferedStream::new(&stream));
				crossbeam::scope(|scope| {
					scope.spawn(|| {
						for (process, args, vars, elf, arg, ports) in receiver {
							let mut stream_write = stream_write.write();
							bincode::serialize_into(&mut stream_write, &process).unwrap();
							bincode::serialize_into(&mut stream_write, &ports).unwrap(); // TODO: do all ports before everything else
							bincode::serialize_into(&mut stream_write, &args).unwrap();
							bincode::serialize_into(&mut stream_write, &vars).unwrap();
							bincode::serialize_into(&mut stream_write, &(elf.len() as u64))
								.unwrap();
							stream_write.write_all(&elf).unwrap();
							bincode::serialize_into(&mut stream_write, &arg).unwrap();
							mem::drop(stream_write);
						}
					});
					scope.spawn(|| {
						let sender = sender;
						while let Ok(done) =
							bincode::deserialize_from::<_, Either<u16, u16>>(&mut stream_read)
								.map_err(map_bincode_err)
						{
							sender.send(Either::Right((i, done))).unwrap();
						}
					});
				});
			});
			for (bridge, ports) in bridges {
				for &port in &ports {
					let check_port = check_addresses.insert(port);
					assert!(check_port);
				}
				let sender = sender.clone();
				thread::spawn(move || {
					let mut path = env::current_exe().unwrap();
					path.pop();
					path.push(&bridge);
					let mut file_in = fs::File::open(&path)
						.or_else(|_| fs::File::open(&bridge))
						.unwrap_or_else(|_| panic!("Failed to open bridge {:?}", &bridge));
					let mut elf = Vec::new();
					file_in.read_to_end(&mut elf).unwrap();
					let (sender_, receiver) = mpsc::sync_channel::<Option<Pid>>(0);
					sender
						.send(Either::Left((
							Resources { mem: 0, cpu: 0.0 },
							vec![ffi::OsString::from(bridge)],
							Vec::new(),
							elf,
							Vec::new(),
							sender_,
							Some(i),
							ports,
						))).unwrap();
					let pid: Option<Pid> = receiver.recv().unwrap();
					println!("bridge at {:?}", pid.unwrap());
				});
			}
			(sender_a, node, addr.ip(), local_addr, VecDeque::new())
		}).collect::<Vec<_>>();

	let listener = net::TcpListener::bind(addr).unwrap();
	thread::spawn(move || {
		for stream in listener.incoming() {
			// println!("accepted");
			let mut stream = stream.unwrap();
			let sender = sender.clone();
			thread::spawn(move || {
				let (mut stream_read, mut stream_write) = (BufferedStream::new(&stream), &stream);
				while let Ok((process, args, vars, elf, arg)) = parse_request(&mut stream_read) {
					// println!("parsed");
					let (sender_, receiver) = mpsc::sync_channel::<Option<Pid>>(0);
					sender
						.send(Either::Left((
							process,
							args,
							vars,
							elf,
							arg,
							sender_,
							None,
							vec![],
						))).unwrap();
					let pid: Option<Pid> = receiver.recv().unwrap();
					// let mut stream_write = stream_write.write();
					if let Err(_) = bincode::serialize_into(&mut stream_write, &pid) {
						break;
					}
				}
			});
		}
	});

	let mut processes: HashMap<(usize, u16), Resources> = HashMap::new();

	for msg in receiver.iter() {
		match msg {
			Either::Left((process, args, vars, elf, arg, sender, force, ports)) => {
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
					).unwrap();
					sched_arg.extend(arg);
					node.0
						.send((process, args, vars, elf, sched_arg, ports))
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
