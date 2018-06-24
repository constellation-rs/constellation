//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "Resources \\{ mem: 20971520, cpu: 0\\.05 \\}\n",
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
//=           "hi Resources \\{ mem: 20971520, cpu: 0\\.001 \\}\n",
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
//=           "hi Resources \\{ mem: 20971521, cpu: 0\\.001 \\}\n",
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
//=           "hi Resources \\{ mem: 20971522, cpu: 0\\.001 \\}\n",
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
//=           "hi Resources \\{ mem: 20971523, cpu: 0\\.001 \\}\n",
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
use std::{env,io,thread,time};
use std::io::{Read,Write};

use deploy::*;

fn sub(parent: Pid, i: u64) {
	assert_eq!(resources().mem, 20*1024*1024+i);
	println!("hi {:?}", resources());
}

fn main() {
	init(Resources{mem:20*1024*1024,..Resources::default()});
	println!("{:?}", resources());
	for i in 0..4 {
		let pid = spawn(sub, i, Resources{mem:20*1024*1024+i,cpu:0.001}).expect("SPAWN FAILED");
	}
}
