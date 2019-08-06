//! A simple example of fork-join parallelism.
//!
//! This example generates random SHA1 hashes for 10 seconds and then prints the
//! lexicographically "lowest" of them.
//!
//! It is invoked like:
//! ```bash
//! cargo run --example fork_join
//! ```
//!
//! By default, 10 processes are spawned (the *fork* of fork-join parallelism).
//! These processes then loop for 10 seconds, hashing random strings, before
//! sending the lowest hash found to the initial process. The initial process
//! collects these lowest hashes (the *join* of fork-join parallelism), and
//! prints the lowest overall.
//!
//! The number of processes is configurable at the command line like so:
//! ```bash
//! cargo run --example fork_join -- 42
//! ```
//! to run for example 42 processes.
//!
//! It can also be run distributed on a [`constellation`](https://github.com/alecmocatta/constellation)
//! cluster like so:
//! ```bash
//! cargo deploy 10.0.0.1 --example fork_join -- 1000
//! ```
//! where `10.0.0.1` is the address of the master. See [here](https://github.com/alecmocatta/constellation)
//! for instructions on setting up the cluster.

use rand::{distributions::Alphanumeric, Rng};
use sha1::Sha1;
use std::{env, iter, time};

use constellation::*;

fn main() {
	init(Resources::default());

	// Accept the number of processes at the command line, defaulting to 10
	let processes = env::args()
		.nth(1)
		.and_then(|arg| arg.parse::<usize>().ok())
		.unwrap_or(10);

	let processes = (0..processes)
		.map(|i| {
			// Spawn the following FnOnce closure in a new process
			let child = spawn(
				// Use the default resource limits, which are enough for this example
				Resources::default(),
				// Make this closure serializable by wrapping with serde_closure's
				// FnOnce!() macro, which requires explicitly listing captured variables.
				serde_closure::FnOnce!([i] move |parent| {
					println!("process {}: commencing hashing", i);

					let mut rng = rand::thread_rng();

					// To record the lowest hash value seen
					let mut lowest: Option<(String,[u8;20])> = None;

					// Loop for ten seconds
					let start = time::Instant::now();
					while start.elapsed() < time::Duration::new(10,0) {
						// Generate a random 7 character string
						let string: String = iter::repeat(()).map(|()| rng.sample(Alphanumeric)).take(7).collect();

						// Hash the string
						let hash = Sha1::from(&string).digest().bytes();

						// Update our record of the lowest hash value seen
						if lowest.is_none() || lowest.as_ref().unwrap().1 >= hash {
							lowest = Some((string,hash));
						}
					}

					let lowest = lowest.unwrap();
					println!("process {}: lowest hash was {} from string \"{}\"", i, hex::encode(lowest.1), lowest.0);

					// Create a `Sender` half of a channel to our parent
					let sender = Sender::<(String,[u8;20])>::new(parent);

					// Send our record along the channel to our parent
					sender.send(lowest);
				}),
			)
			.expect("Unable to allocate process!");

			// Create a `Receiver` half of a channel to the newly-spawned child
			Receiver::<(String, [u8; 20])>::new(child)
		})
		.collect::<Vec<_>>();

	// `processes` is now a Vec of `Receiver`s

	let result = processes
		.into_iter()
		// Receive a record from each `Receiver`
		.map(|receiver| receiver.recv().unwrap())
		// Take the record with the lowest hash
		.min_by_key(|&(_, hash)| hash)
		.unwrap();

	println!(
		"overall lowest hash was {} from string \"{}\"",
		hex::encode(result.1),
		result.0
	);
}
