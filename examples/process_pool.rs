//! A simple example of a process pool.
//!
//! This example spawns a process pool, and distributes work across it.
//!
//! It is invoked like:
//! ```bash
//! cargo run --example process_pool
//! ```
//!
//! By default, 10 processes are spawned for the pool. By default, 30 jobs –
//! which sleep for a couple of seconds before returning a `String` – are
//! spawned on the pool. As such they are round-robin allocated to the 10
//! processes of the pool. The initial process collects and prints the `String`s
//! returned by each job.
//!
//! This is a simple implementation; a more featureful version is [`amadeus`](https://github.com/alecmocatta/amadeus).
//!
//! The number of processes is configurable at the command line like so:
//! ```bash
//! cargo run --example process_pool -- 42
//! ```
//! to run for example 42 processes.
//!
//! It can also be run distributed on a [`constellation`](https://github.com/alecmocatta/constellation)
//! cluster like so:
//! ```bash
//! cargo deploy 10.0.0.1 --example process_pool -- 1000
//! ```
//! where `10.0.0.1` is the address of the master. See [here](https://github.com/alecmocatta/constellation)
//! for instructions on setting up the cluster.

#![allow(where_clauses_object_safety, clippy::type_complexity)]

use rand::Rng;
use serde_traitobject as st;
use std::{any, collections::VecDeque, env, marker, mem, thread, time};

use constellation::*;

// type Request = st::Box<dyn st::FnOnce() -> Response>; // when #![feature(unboxed_closures)] is stable
type Request = st::Box<dyn st::FnOnce<(), Output = Response>>;
type Response = st::Box<dyn st::Any>;

struct Process {
	sender: Sender<Option<Request>>,
	receiver: Receiver<Response>,
	queue: VecDeque<Queued<Response>>,
	received: usize,
	tail: usize,
}

enum Queued<T> {
	Awaiting,
	Got(T),
	Taken,
}
impl<T> Queued<T> {
	fn received(&mut self, t: T) {
		if let Queued::Awaiting = mem::replace(self, Queued::Got(t)) {
		} else {
			panic!()
		}
	}
	fn take(&mut self) -> T {
		if let Queued::Got(t) = mem::replace(self, Queued::Taken) {
			t
		} else {
			panic!()
		}
	}
}

struct ProcessPool {
	processes: Vec<Process>,
	i: usize,
}
impl ProcessPool {
	fn new(processes: usize, resources: Resources) -> Self {
		let processes = (0..processes)
			.map(|_| {
				// Spawn the following FnOnce closure in a new process
				let child = spawn(
					// Use the default resource limits, which are enough for this example
					resources,
					// Make this closure serializable by wrapping with serde_closure's
					// FnOnce!() macro, which requires explicitly listing captured variables.
					serde_closure::FnOnce!([] move |parent| {
						// println!("process {}: awaiting work", i);

						// Create a `Sender` half of a channel to our parent
						let receiver = Receiver::<Option<Request>>::new(parent);

						// Create a `Sender` half of a channel to our parent
						let sender = Sender::<Response>::new(parent);

						while let Some(work) = receiver.recv().block().unwrap() {
							// println!("process {}: got work", i);
							let ret = work();
							// println!("process {}: done work", i);
							sender.send(ret).block();
							// println!("process {}: awaiting work", i);
						}
					}),
				)
				.expect("Unable to allocate process!");

				// Create a `Receiver` half of a channel to the newly-spawned child
				let sender = Sender::new(child);
				let receiver = Receiver::new(child);

				let (queue, received, tail) = (VecDeque::new(), 0, 0);

				Process {
					sender,
					receiver,
					queue,
					received,
					tail,
				}
			})
			.collect::<Vec<_>>();
		ProcessPool { processes, i: 0 }
	}
	fn spawn<
		F: FnOnce() -> T + serde::ser::Serialize + serde::de::DeserializeOwned + 'static,
		T: any::Any + serde::ser::Serialize + serde::de::DeserializeOwned,
	>(
		&mut self, work: F,
	) -> JoinHandle<T> {
		let process_index = self.i;
		self.i += 1;
		if self.i == self.processes.len() {
			self.i = 0;
		}
		let process = &mut self.processes[process_index];
		process
			.sender
			.send(Some(st::Box::new(serde_closure::FnOnce!([work] move || {
				let work: F = work;
				st::Box::new(work()) as Response
			})) as Request))
			.block();
		process.queue.push_back(Queued::Awaiting);
		JoinHandle(
			process_index,
			process.tail + process.queue.len() - 1,
			marker::PhantomData,
		)
	}
	fn join<T: any::Any>(&mut self, key: JoinHandle<T>) -> T {
		let JoinHandle(process_index, process_offset, _) = key;
		drop(key); // placate clippy::needless_pass_by_value
		let process = &mut self.processes[process_index];
		while process.received <= process_offset {
			process.queue[process.received - process.tail]
				.received(process.receiver.recv().block().unwrap());
			process.received += 1;
		}
		let boxed: st::Box<_> = process.queue[process_offset - process.tail].take();
		while let Some(Queued::Taken) = process.queue.front() {
			let _ = process.queue.pop_front().unwrap();
			process.tail += 1;
		}
		*Box::<dyn any::Any>::downcast::<T>(boxed.into_any()).unwrap()
	}
}
impl Drop for ProcessPool {
	fn drop(&mut self) {
		for Process { sender, .. } in &self.processes {
			sender.send(None).block();
		}
	}
}

struct JoinHandle<T: any::Any>(usize, usize, marker::PhantomData<fn() -> T>);

fn main() {
	init(Resources::default());

	// Accept the number of processes at the command line, defaulting to 10
	let processes = env::args()
		.nth(1)
		.and_then(|arg| arg.parse::<usize>().ok())
		.unwrap_or(10);

	let mut pool = ProcessPool::new(processes, Resources::default());

	let handles = (0..processes * 3)
		.map(|i| {
			pool.spawn(serde_closure::FnOnce!([i] move || -> String {
				thread::sleep(rand::thread_rng().gen_range(time::Duration::new(0,0),time::Duration::new(5,0)));
				format!("warm greetings from job {}", i)
			}))
		})
		.collect::<Vec<_>>();

	for handle in handles {
		println!("{}", pool.join(handle));
	}
}
