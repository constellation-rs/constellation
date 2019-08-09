//! A simple example of all-to-all communication.
//!
//! This example spawns several processes, which then send each other a message.
//! The communication is all-to-all; if there are `n` processes spawned, there
//! will be `n(n-1)` messages.
//!
//! It is invoked like:
//! ```bash
//! cargo run --example all_to_all
//! ```
//!
//! By default, 10 processes are spawned. They are then sent the pids of the
//! rest of the 10 processes. Messages – a tuple of `(source process index,
//! destination process index)` – are then sent in a carefully ordered manner to
//! ensure that sends and receives are synchronised to avoid deadlocking. The
//! processes then print "done" and exit.
//!
//! This is a naïve implementation; a better one would leverage asynchrony.
//! There will be such an example shortly.
//!
//! The number of processes is configurable at the command line like so:
//! ```bash
//! cargo run --example all_to_all -- 42
//! ```
//! to run for example 42 processes.
//!
//! It can also be run distributed on a [`constellation`](https://github.com/alecmocatta/constellation)
//! cluster like so:
//! ```bash
//! cargo deploy 10.0.0.1 --example all_to_all -- 1000
//! ```
//! where `10.0.0.1` is the address of the master. See [here](https://github.com/alecmocatta/constellation)
//! for instructions on setting up the cluster.

use std::env;

use constellation::*;

fn main() {
	init(Resources::default());

	// Accept the number of processes at the command line, defaulting to 10
	let processes = env::args()
		.nth(1)
		.and_then(|arg| arg.parse::<usize>().ok())
		.unwrap_or(10);

	let pids: Vec<Pid> = (0..processes)
		.map(|process_index| {
			spawn(
				Resources {
					mem: 20 * 1024 * 1024,
					..Resources::default()
				},
				serde_closure::FnOnce!([process_index] move |parent| {
					let receiver = Receiver::<Vec<Pid>>::new(parent);
					let pids = receiver.recv().block().unwrap();
					assert_eq!(pids[process_index], pid());
					let mut senders: Vec<Option<Sender<usize>>> = Vec::with_capacity(pids.len());
					let mut receivers: Vec<Option<Receiver<usize>>> = Vec::with_capacity(pids.len());
					for i in 0..pids.len() {
						for j in 0..pids.len() {
							if i == process_index {
								senders.push(if i != j {
									Some(Sender::new(pids[j]))
								} else {
									None
								});
							}
							if j == process_index {
								receivers.push(if i != j {
									Some(Receiver::new(pids[i]))
								} else {
									None
								});
							}
						}
					}
					for (i,receiver) in receivers.iter().enumerate() {
						for (j,sender) in senders.iter().enumerate() {
							if i == j {
								continue;
							}
							if i == process_index {
								sender.as_ref().unwrap().send(i * j).block();
							}
							if j == process_index {
								let x = receiver.as_ref().unwrap().recv().block().unwrap();
								assert_eq!(x, i * j);
							}
						}
					}
					println!("done");
				}),
			)
			.expect("Spawn failed")
		})
		.collect();

	let senders: Vec<Sender<std::vec::Vec<Pid>>> =
		pids.iter().map(|&pid| Sender::new(pid)).collect();

	for sender in senders {
		sender.send(pids.clone()).block();
	}
}
