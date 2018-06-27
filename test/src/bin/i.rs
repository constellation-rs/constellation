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
//=       "exit": {
//=         "Left": 0
//=       }
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
//=               "exit": {
//=                 "Left": 0
//=               }
//=             }
//=           ],
//=           "exit": {
//=             "Left": 0
//=           }
//=         }
//=       ],
//=       "exit": {
//=         "Left": 0
//=       }
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
//=       "exit": {
//=         "Left": 0
//=       }
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
//=       "exit": {
//=         "Left": 0
//=       }
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
//=       "exit": {
//=         "Left": 0
//=       }
//=     }
//=   ],
//=   "exit": {
//=     "Left": 0
//=   }
//= }

#![deny(warnings, deprecated)]
extern crate deploy;
extern crate serde;
use deploy::*;
use std::{env, thread, time};

fn sub<T: std::fmt::Debug>(parent: Pid, arg: T) {
	println!("! sub1");
	let receiver = Receiver::new(parent);
	let hi: String = receiver.recv().unwrap();
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
}
fn sub2<
	T: std::fmt::Display + for<'de> serde::de::Deserialize<'de> + serde::ser::Serialize + 'static,
>(
	parent: Pid, arg: (usize, T),
) {
	println!("! sub2");
	if arg.0 != 0 {
		let child_pid = spawn(
			sub2,
			(arg.0 - 1, format!("hello alec! {}; {}", arg.0 - 1, arg.1)),
			Resources {
				mem: 20 * 1024 * 1024,
				..Resources::default()
			},
		).expect("SPAWN FAILED");
		let receiver = Receiver::<T>::new(child_pid);
		let sender = Sender::new(parent);
		sender.send(receiver.recv().unwrap()).unwrap();
	} else {
		// if unsafe{fork()} == 0 {
		// 	loop{}
		// }
		let sender = Sender::new(parent);
		sender.send(arg.1).unwrap();
	}
	// println!("PID!!! {:?}", unsafe{getpid()});
	// std::thread::sleep(std::time::Duration::new(200,0));
	// loop {
	// 	thread::sleep_ms(1000);
	// }
	std::process::exit(0);
}
fn sub3(parent: Pid, arg: usize) {
	println!("! sub3");
	let receiver = Receiver::<Vec<Pid>>::new(parent);
	let pids = receiver.recv().unwrap();
	// println!("{:?}", pids);
	assert_eq!(pids[arg], pid());
	let mut senders: Vec<Option<Sender<usize>>> = Vec::with_capacity(pids.len() - 1);
	let mut receivers: Vec<Option<Receiver<usize>>> = Vec::with_capacity(pids.len() - 1);
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
	for i in 0..pids.len() {
		for j in 0..pids.len() {
			if i == j {
				continue;
			}
			if i == arg {
				senders[j].as_ref().unwrap().send(i * j).unwrap();
			}
			if j == arg {
				let x = receivers[i].as_ref().unwrap().recv().unwrap();
				assert_eq!(x, i * j);
			}
		}
	}
	println!("done2");
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
			let pid = spawn(
				sub,
				(String::from("HELLO!!!"), String::from("THERE!!!")),
				Resources {
					mem: 20 * 1024 * 1024,
					..Resources::default()
				},
			).expect("SPAWN FAILED");
			let sender = Sender::new(pid);
			sender.send(format!("hello alec! {}", i)).unwrap();
		}
	});
	let b = thread::spawn(move || {
		let count = b;
		let pid = spawn(
			sub2,
			(count, format!("hello alec! {}", count)),
			Resources {
				mem: 20 * 1024 * 1024,
				..Resources::default()
			},
		).expect("SPAWN FAILED");
		let receiver = Receiver::<String>::new(pid);
		println!("final: {:?}", receiver.recv().unwrap());
	});
	let c = thread::spawn(move || {
		let count = c;
		let pids: Vec<Pid> = (0..count)
			.map(|i| {
				spawn(
					sub3,
					i,
					Resources {
						mem: 20 * 1024 * 1024,
						..Resources::default()
					},
				).expect("SPAWN FAILED")
			})
			.collect();
		let senders: Vec<Sender<std::vec::Vec<deploy::Pid>>> =
			pids.iter().map(|&pid| Sender::new(pid)).collect();
		for sender in senders {
			sender.send(pids.clone()).unwrap();
		}
	});
	a.join().unwrap();
	b.join().unwrap();
	c.join().unwrap();
}
