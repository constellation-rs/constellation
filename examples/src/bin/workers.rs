extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate deploy;
use deploy::*;

fn main() {
	init(Resources::default());

	let mut total = 0;
	for index in 0..10 {
		let greeting = format!("hello worker {}!", index);
		let worker_arg = WorkerArg { index, greeting };
		let pid = spawn(worker, worker_arg, Resources::default()).expect("Out of resources!");
		let receiver = Receiver::<usize>::new(pid);
		total += receiver.recv().unwrap();
	}

	println!("total {}!", total);
}

#[derive(Serialize, Deserialize)]
struct WorkerArg {
	index: usize,
	greeting: String,
}

fn worker(parent: Pid, worker_arg: WorkerArg) {
	println!("{}", worker_arg.greeting);

	let sender = Sender::<usize>::new(parent);
	sender.send(worker_arg.index * 100).unwrap();
}
