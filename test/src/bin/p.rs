//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "hi\nhi\n",
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
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
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
	thread, time,
};

use deploy::*;

fn sub<T>(parent: Pid, arg: T) {
	let sender = Sender::<String>::new(parent);
	sender.send(String::from("hi")).unwrap();
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
		let receiver = Receiver::<String>::new(pid);
		println!("{}", receiver.recv().unwrap());
	}
}
