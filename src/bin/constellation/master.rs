#![allow(clippy::too_many_lines)]

use either::Either;
use serde::Serialize;
use std::{
	collections::{HashMap, VecDeque}, env, ffi::OsString, net::{IpAddr, SocketAddr, TcpListener, TcpStream}, sync::mpsc::{sync_channel, SyncSender}, thread
};

use constellation_internal::{
	abort_on_unwind, abort_on_unwind_1, map_bincode_err, msg::{bincode_deserialize_from, FabricRequest}, BufferedStream, Pid, Resources
};

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

pub fn run(
	bind_addr: SocketAddr, master_pid: Pid,
	nodes: HashMap<SocketAddr, (Option<SocketAddr>, u64, u32)>,
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
		.map(|(i, (fabric, (bridge, mem, cpu)))| {
			let node = Node { mem, cpu };
			let (sender_a, receiver_a) = sync_channel::<FabricRequest<Vec<u8>, Vec<u8>>>(0);
			let stream = TcpStream::connect(&fabric)
				.unwrap_or_else(|e| panic!("couldn't connect to node {}: {:?}: {}", i, fabric, e));
			let sender1 = sender.clone();
			let _ = thread::Builder::new()
				.spawn(abort_on_unwind(move || {
					let (receiver, sender) = (receiver_a, sender1);
					let (mut stream_read, mut stream_write) =
						(BufferedStream::new(&stream), BufferedStream::new(&stream));
					bincode::serialize_into::<_, IpAddr>(&mut stream_write, &fabric.ip()).unwrap();
					let _ip = bincode::deserialize_from::<_, IpAddr>(&mut stream_read).unwrap();
					crossbeam::scope(|scope| {
						let _ = scope.spawn(abort_on_unwind_1(|_spawn| {
							for request in receiver {
								bincode::serialize_into(&mut stream_write.write(), &request)
									.unwrap();
							}
						}));
						while let Ok(done) =
							bincode::deserialize_from::<_, Either<Pid, Pid>>(&mut stream_read)
								.map_err(map_bincode_err)
						{
							sender.send(Either::Right((i, done))).unwrap();
						}
					})
					.unwrap();
				}))
				.unwrap();
			if let Some(bridge) = bridge {
				let sender = sender.clone();
				let _ = thread::Builder::new()
					.spawn(abort_on_unwind(move || {
						#[cfg(feature = "distribute_binaries")]
						let binary = {
							let mut binary = Vec::new();
							let mut file_in = palaver::env::exe().unwrap();
							let _ = std::io::Read::read_to_end(&mut file_in, &mut binary).unwrap();
							binary
						};
						#[cfg(not(feature = "distribute_binaries"))]
						let binary = std::marker::PhantomData;
						let (sender_, receiver) = sync_channel::<Option<Pid>>(0);
						sender
							.send(Either::Left((
								FabricRequest {
									resources: Resources { mem: 0, cpu: 0 },
									bind: vec![bridge],
									args: vec![
										OsString::from(env::current_exe().unwrap()),
										OsString::from("bridge"),
									],
									vars: Vec::new(),
									binary,
									arg: Vec::new(),
								},
								sender_,
								Some(i),
							)))
							.unwrap();
						let _pid: Pid = receiver.recv().unwrap().unwrap();
						// println!("bridge at {:?}", pid);
					}))
					.unwrap();
			}
			(sender_a, node, fabric.ip(), VecDeque::new())
		})
		.collect::<Vec<_>>();

	let listener = TcpListener::bind(bind_addr).unwrap();
	let _ = thread::Builder::new()
		.spawn(abort_on_unwind(move || {
			for stream in listener.incoming() {
				// println!("accepted");
				if stream.is_err() {
					continue;
				}
				let stream = stream.unwrap();
				let sender = sender.clone();
				let _ = thread::Builder::new()
					.spawn(abort_on_unwind(move || {
						let (mut stream_read, mut stream_write) =
							(BufferedStream::new(&stream), &stream);
						while let Ok(request) =
							bincode_deserialize_from(&mut stream_read).map_err(map_bincode_err)
						{
							// println!("parsed");
							let (sender_, receiver) = sync_channel::<Option<Pid>>(0);
							sender.send(Either::Left((request, sender_, None))).unwrap();
							let pid: Option<Pid> = receiver.recv().unwrap();
							// let mut stream_write = stream_write.write();
							if bincode::serialize_into(&mut stream_write, &pid).is_err() {
								break;
							}
						}
					}))
					.unwrap();
			}
		}))
		.unwrap();

	let mut processes: HashMap<(usize, Pid), Resources> = HashMap::new();

	for msg in receiver.iter() {
		match msg {
			Either::Left((mut request, sender, force)) => {
				// println!("spawn {:?}", request.resources);
				let node = force.or_else(|| {
					nodes
						.iter()
						.position(|node| node.1.fits(&request.resources))
				});
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
