//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "1234567890\n1234567890\n",
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
//=           "hi\n",
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
//=           "hi\n",
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
	thread, time,
};

use deploy::*;

fn sub<T>(parent: Pid, arg: T) {
	let receiver = Receiver::<String>::new(parent);
	let sender = Sender::<usize>::new(parent);
	println!("{}", receiver.recv().unwrap());
	sender.send(1234567890).unwrap();
}

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	for i in 0..2 {
		let pid = spawn(
			sub,
			(),
			Resources {
				mem: 20 * 1024 * 1024,
				..Resources::default()
			},
		).expect("SPAWN FAILED");
		let sender = Sender::<String>::new(pid);
		let receiver = Receiver::<usize>::new(pid);
		sender.send(String::from("hi")).unwrap();
		println!("{}", receiver.recv().unwrap());
	}
}
