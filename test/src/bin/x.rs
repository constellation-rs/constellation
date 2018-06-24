//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "\\[\"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\"\\]\n",
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
//=           "hi 4\n",
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
//=           "hi 6\n",
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
//=           "hi 7\n",
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
//=           "hi 9\n",
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
//=           "hi 1\n",
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
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi 3\n",
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
//=           "hi 0\n",
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
//=     }
//=   ],
//=   "exit": {
//=     "Left": 0
//=   }
//= }

#![feature(never_type)]
extern crate deploy;
extern crate serde;
extern crate futures;
use std::{env,io,thread,time};
use std::io::{Read,Write};

use deploy::*;
use futures::stream::StreamExt;
use futures::future::FutureExt;

fn sub(parent: Pid, arg: u32) {
	println!("hi {}", arg);
	let _ = futures::executor::block_on(Receiver::<String>::new(parent).take(5).forward(Sender::<String>::new(parent))).unwrap();
}

fn main() {
	init(Resources{mem:20*1024*1024,..Resources::default()});
	let x = futures::executor::block_on(futures::future::join_all((0..10).map(|i|{
		let pid = spawn(sub, i, Resources{mem:20*1024*1024,..Resources::default()}).expect("SPAWN FAILED");
		futures::stream::iter_ok(vec![String::from("abc"),String::from("def"),String::from("ghi"),String::from("jkl"),String::from("mno")]).forward(Sender::<String>::new(pid))
		.join(Receiver::<String>::new(pid).take(5).fold(String::new(), |acc,x|futures::future::ok(acc+&x)))
		.map(|(_,res)|res)
	}))).unwrap();
	println!("{:?}", x);
}
