use constellation::*;
use futures::future::join;
use serde_closure::FnOnce;
use serde_traitobject as st;

fn fib_threads(x: usize) -> usize {
	if x <= 1 {
		return x;
	}
	let a = std::thread::spawn(move || fib_threads(x - 1));
	let b = std::thread::spawn(move || fib_threads(x - 2));
	let a = a.join().unwrap();
	let b = b.join().unwrap();
	a + b
}

fn fib_processes(x: usize) -> usize {
	if x <= 1 {
		return x;
	}
	let left_pid = spawn(
		Resources::default(),
		FnOnce!(move |parent_pid| {
			Sender::<usize>::new(parent_pid)
				.send(fib_processes(x - 1))
				.block()
		}),
	)
	.block()
	.unwrap();

	let right_pid = spawn(
		Resources::default(),
		FnOnce!(move |parent_pid| {
			Sender::<usize>::new(parent_pid)
				.send(fib_processes(x - 2))
				.block()
		}),
	)
	.block()
	.unwrap();

	Receiver::<usize>::new(left_pid).recv().block().unwrap()
		+ Receiver::<usize>::new(right_pid).recv().block().unwrap()
}

fn fib_processes_async(x: usize) -> Result<usize, Box<dyn st::Error>> {
	type Msg = Result<usize, Box<dyn st::Error>>;
	if x <= 1 {
		return Ok(x);
	}
	let left = async {
		let pid = spawn(
			Resources::default(),
			FnOnce!(move |parent| {
				Sender::<Msg>::new(parent)
					.send(fib_processes_async(x - 1))
					.block()
			}),
		)
		.await?;
		let receiver = Receiver::<Msg>::new(pid);
		receiver.recv().await?
	};

	let right = async {
		let pid = spawn(
			Resources::default(),
			FnOnce!(move |parent| {
				Sender::<Msg>::new(parent)
					.send(fib_processes_async(x - 2))
					.block()
			}),
		)
		.await?;
		let receiver = Receiver::<Msg>::new(pid);
		receiver.recv().await?
	};

	let (left, right) = join(left, right).block();
	Ok(left? + right?)
}

fn main() {
	init(Resources::default());
	for i in 0..10 {
		println!("{}", fib_threads(i));
	}
	for i in 0..10 {
		println!("{}", fib_processes(i));
	}
	for i in 0..10 {
		println!("{}", fib_processes_async(i).unwrap());
	}
}
