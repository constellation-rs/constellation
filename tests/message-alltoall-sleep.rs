//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "DEPLOY_MEM: Err\\(NotPresent\\)\nfinal: \"hello alec! 0; hello alec! 1; hello alec! 2\"\n",
//=       true
//=     ]
//=   },
//=   "children": [
//=     {
//=       "output": {
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "! sub1\nhello alec! 0 : \\(\"HELLO!!!\", \"THERE!!!\"\\)\n0\n1\n2\n3\n4\ndone: hello alec! 0\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "! sub2\n",
//=           true
//=         ]
//=       },
//=       "children": [
//=         {
//=           "output": {
//=             "2": [
//=               "",
//=               true
//=             ],
//=             "1": [
//=               "! sub2\n",
//=               true
//=             ]
//=           },
//=           "children": [
//=             {
//=               "output": {
//=                 "2": [
//=                   "",
//=                   true
//=                 ],
//=                 "1": [
//=                   "! sub2\n",
//=                   true
//=                 ]
//=               },
//=               "children": [],
//=               "exit": "Success"
//=             }
//=           ],
//=           "exit": "Success"
//=         }
//=       ],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "! sub3\ndone\ndone2\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "! sub3\ndone\ndone2\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "! sub1\nhello alec! 1 : \\(\"HELLO!!!\", \"THERE!!!\"\\)\n0\n1\n2\n3\n4\ndone: hello alec! 1\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     }
//=   ],
//=   "exit": "Success"
//= }

use constellation::*;
use serde_closure::FnOnce;
use std::{env, thread, time};

fn sub2<
	T: std::fmt::Display + for<'de> serde::de::Deserialize<'de> + serde::ser::Serialize + 'static,
>(
	parent: Pid, arg: (usize, T),
) {
	println!("! sub2");
	if arg.0 != 0 {
		let arg = (arg.0 - 1, format!("hello alec! {}; {}", arg.0 - 1, arg.1));
		let child_pid = spawn(
			Resources {
				mem: 20 * 1024 * 1024,
				..Resources::default()
			},
			FnOnce!([arg] move |parent| {
				sub2(parent, arg)
			}),
		)
		.block()
		.expect("spawn() failed to allocate process");
		let receiver = Receiver::<T>::new(child_pid);
		let sender = Sender::new(parent);
		sender.send(receiver.recv().block().unwrap()).block();
	} else {
		// if unsafe{fork()} == 0 {
		// 	loop{}
		// }
		let sender = Sender::new(parent);
		sender.send(arg.1).block();
	}
	// println!("PID!!! {:?}", unsafe{getpid()});
	// std::thread::sleep(std::time::Duration::new(200,0));
	// loop {
	// 	thread::sleep_ms(1000);
	// }
	std::process::exit(0);
}

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	std::env::set_var("RUST_BACKTRACE", "full");
	println!("DEPLOY_MEM: {:?}", env::var("DEPLOY_MEM"));
	// unsafe{
	// asm!("int3;nop;");
	// raise(6);
	// std::intrinsics::abort();
	// _exit(123);
	// }
	// process::exit(123);

	// let mut args = env::args(); args.next().unwrap();
	// let mut x = Vec::new();
	// io::stdin().read_to_end(&mut x).unwrap();
	// println!("STARTING {:?}", std::str::from_utf8(&x).unwrap());
	let a = 2; //args.next().unwrap().parse().unwrap();
	let b = 2; //args.next().unwrap().parse().unwrap();
	let c = 2; //args.next().unwrap().parse().unwrap();
	let a = thread::spawn(move || {
		for i in 0..a {
			let arg = (String::from("HELLO!!!"), String::from("THERE!!!"));
			let pid = spawn(
				Resources {
					mem: 20 * 1024 * 1024,
					..Resources::default()
				},
				FnOnce!([arg] move |parent| {
					println!("! sub1");
					let receiver = Receiver::new(parent);
					let hi: String = receiver.recv().block().unwrap();
					println!("{} : {:?}", hi, arg);
					for i in 0..5 {
						thread::sleep(time::Duration::new(0, 500_000_000));
						println!("{:?}", i);
					}
					// for i in 0..5_000_000_000usize {
					// 	if i % 1_000_000_000 == 0 {
					// 		println!("{:?}", i);
					// 		io::stdout().flush().unwrap();
					// 		eprintln!("{:?}", i);
					// 		io::stderr().flush().unwrap();
					// 	}
					// }
					println!("done: {}", hi);
				}),
			)
			.block()
			.expect("spawn() failed to allocate process");
			let sender = Sender::new(pid);
			sender.send(format!("hello alec! {}", i)).block();
		}
	});
	let b = thread::spawn(move || {
		let count = b;
		let arg = (count, format!("hello alec! {}", count));
		let pid = spawn(
			Resources {
				mem: 20 * 1024 * 1024,
				..Resources::default()
			},
			FnOnce!([arg] move |parent| {
				sub2(parent, arg)
			}),
		)
		.block()
		.expect("spawn() failed to allocate process");
		let receiver = Receiver::<String>::new(pid);
		println!("final: {:?}", receiver.recv().block().unwrap());
	});
	let c = thread::spawn(move || {
		let count = c;
		let pids: Vec<Pid> = (0..count)
			.map(|arg| {
				spawn(
					Resources {
						mem: 20 * 1024 * 1024,
						..Resources::default()
					},
					FnOnce!([arg] move |parent| {
						println!("! sub3");
						let receiver = Receiver::<Vec<Pid>>::new(parent);
						let pids = receiver.recv().block().unwrap();
						// println!("{:?}", pids);
						assert_eq!(pids[arg], pid());
						let mut senders: Vec<Option<Sender<usize>>> = Vec::with_capacity(pids.len());
						let mut receivers: Vec<Option<Receiver<usize>>> = Vec::with_capacity(pids.len());
						for i in 0..pids.len() {
							for j in 0..pids.len() {
								if i == arg {
									senders.push(if i != j {
										Some(Sender::new(pids[j]))
									} else {
										None
									});
								}
								if j == arg {
									receivers.push(if i != j {
										Some(Receiver::new(pids[i]))
									} else {
										None
									});
								}
							}
						}
						assert_eq!(senders.len(), pids.len());
						assert_eq!(receivers.len(), pids.len());
						println!("done");
						for (i,receiver) in receivers.iter().enumerate() {
							for (j,sender) in senders.iter().enumerate() {
								if i == j {
									continue;
								}
								if i == arg {
									sender.as_ref().unwrap().send(i * j).block();
								}
								if j == arg {
									let x = receiver.as_ref().unwrap().recv().block().unwrap();
									assert_eq!(x, i * j);
								}
							}
						}
						println!("done2");
					}),
				)
				.block()
				.expect("spawn() failed to allocate process")
			})
			.collect();
		let senders: Vec<Sender<std::vec::Vec<Pid>>> =
			pids.iter().map(|&pid| Sender::new(pid)).collect();
		for sender in senders {
			sender.send(pids.clone()).block();
		}
	});
	a.join().unwrap();
	b.join().unwrap();
	c.join().unwrap();
}
