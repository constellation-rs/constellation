use bincode;
use crossbeam;
use either::Either;
use palaver::file::copy;
use serde::Serialize;
use std::{
	collections::{HashMap, HashSet, VecDeque}, convert::{TryFrom, TryInto}, env, ffi::OsString, fs, io::{self, Read}, net::{self, IpAddr}, path, sync::mpsc::{sync_channel, SyncSender}, thread
};

use constellation_internal::{map_bincode_err, msg::FabricRequest, BufferedStream, Pid, Resources};

#[derive(Debug)]
pub struct Node {
	mem: u64,
	cpu: u32,
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
	bind_addr: net::SocketAddr, master_pid: Pid,
	nodes: HashMap<net::SocketAddr, (u64, u32, Vec<(path::PathBuf, Vec<net::SocketAddr>)>)>,
) {
	let (sender, receiver) = sync_channel::<
		Either<
			(
				FabricRequest<Vec<u8>, Vec<u8>>,
				SyncSender<Option<Pid>>,
				Option<usize>,
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
			let (sender_a, receiver_a) = sync_channel::<FabricRequest<Vec<u8>, Vec<u8>>>(0);
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
							for request in receiver {
								bincode::serialize_into(&mut stream_write.write(), &request)
									.unwrap();
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
			for (bridge, bind) in bridges {
				for &port in &bind {
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
						let (sender_, receiver) = sync_channel::<Option<Pid>>(0);
						sender
							.send(Either::Left((
								FabricRequest {
									resources: Resources { mem: 0, cpu: 0 },
									bind,
									args: vec![OsString::from(bridge)],
									vars: Vec::new(),
									binary,
									arg: Vec::new(),
								},
								sender_,
								Some(i),
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
						while let Ok((resources, args, vars, binary, arg)) =
							parse_request(&mut stream_read)
						{
							// println!("parsed");
							let (sender_, receiver) = sync_channel::<Option<Pid>>(0);
							sender
								.send(Either::Left((
									FabricRequest {
										resources,
										bind: vec![],
										args,
										vars,
										arg,
										binary,
									},
									sender_,
									None,
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
			Either::Left((mut request, sender, force)) => {
				// println!("spawn {:?}", request.resources);
				let node = if force.is_none() {
					nodes
						.iter()
						.position(|node| node.1.fits(&request.resources))
				} else {
					Some(force.unwrap())
				};
				if let Some(node) = node {
					let node = &mut nodes[node];
					node.1.alloc(&request.resources);

					let mut arg = Vec::new();
					bincode::serialize_into(
						&mut arg,
						&SchedulerArg {
							ip: node.2,
							scheduler: master_pid,
						},
					)
					.unwrap();
					arg.extend(request.arg);
					request.arg = arg;
					node.3.push_back((sender, request.resources));
					node.0.send(request).unwrap();
				} else {
					// println!(
					// 	"Failing a spawn! Cannot allocate process {:#?} to nodes {:#?}",
					// 	resources, nodes
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
