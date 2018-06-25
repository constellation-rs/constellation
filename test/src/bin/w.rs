//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "",
//=       true
//=     ]
//=   },
//=   "children": [
//=     {
//=       "output": {
//=         "1": [
//=           "hi 3\n",
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
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi 8\n",
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
//=           "hi 4\n",
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
//=           "hi 2\n",
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
//=           "hi 6\n",
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
//=           "hi 9\n",
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
//=           "hi 5\n",
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
//=           "hi 0\n",
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
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi 1\n",
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
//=           "hi 7\n",
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

fn sub(parent: Pid, arg: u32) {
	thread::sleep(time::Duration::new(0, 10_000_000) * arg);
	println!("hi {}", arg);
}

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	for i in 0..10 {
		let pid = spawn(
			sub,
			i,
			Resources {
				mem: 20 * 1024 * 1024,
				..Resources::default()
			},
		).expect("SPAWN FAILED");
	}
}
