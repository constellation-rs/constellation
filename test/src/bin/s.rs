//= {
//=   "output": {
//=     "1": [
//=       "1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\nho\nho\nho\nho\nho\nho\nho\nho\nho\nho\n",
//=       true
//=     ],
//=     "2": [
//=       "",
//=       true
//=     ]
//=   },
//=   "children": [
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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
//=           "hi\n987654321\n",
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

extern crate deploy;
extern crate serde;
use std::{
	env,
	io::{self, Read, Write},
	mem, thread, time,
};

use deploy::*;

fn sub<T>(parent: Pid, arg: T) {
	let receiver = Receiver::<String>::new(parent);
	let sender = Sender::<usize>::new(parent);
	println!("{}", receiver.recv().unwrap());
	sender.send(1234567890).unwrap();
	mem::drop((receiver, sender));
	let receiver = Receiver::<usize>::new(parent);
	let sender = Sender::<String>::new(parent);
	sender.send(String::from("ho")).unwrap();
	println!("{}", receiver.recv().unwrap());
}

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	let pids = (0..10)
		.map(|_| {
			spawn(
				sub,
				(),
				Resources {
					mem: 20 * 1024 * 1024,
					..Resources::default()
				},
			).expect("SPAWN FAILED")
		})
		.collect::<Vec<_>>();
	let channels = pids
		.iter()
		.map(|&pid| (Sender::<String>::new(pid), Receiver::<usize>::new(pid)))
		.collect::<Vec<_>>();
	for &(ref sender, ref receiver) in channels.iter() {
		sender.send(String::from("hi")).unwrap();
	}
	for &(ref sender, ref receiver) in channels.iter() {
		println!("{}", receiver.recv().unwrap());
	}
	mem::drop(channels);
	let channels = pids
		.iter()
		.map(|&pid| (Sender::<usize>::new(pid), Receiver::<String>::new(pid)))
		.collect::<Vec<_>>();
	for &(ref sender, ref receiver) in channels.iter() {
		println!("{}", receiver.recv().unwrap());
	}
	for &(ref sender, ref receiver) in channels.iter() {
		sender.send(0987654321).unwrap();
	}
}
