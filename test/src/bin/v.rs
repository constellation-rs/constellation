//= {
//=   "output": {
//=     "2": [
//=       "thread 'main' panicked at 'Sender::<usize>::new\\(\\) called for pid [a-z0-9]{7} when a Sender to this pid already exists', deploy/src/lib\\.rs:[0-9]+:[0-9]+\n",
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
//=           "",
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
//=     "Right": "SIGABRT"
//=   }
//= }

extern crate deploy;
use std::{panic,process,thread};
use deploy::*;

fn sub<T>(parent: Pid, arg: T) {
}

fn main() {
	init(Resources{mem:20*1024*1024,..Resources::default()});
	panic::set_hook(Box::new(|info|{
		eprintln!("thread '{}' {}", thread::current().name().unwrap(), info);
		process::abort()
	}));
	let pid = spawn(sub, (), Resources{mem:20*1024*1024,..Resources::default()}).expect("SPAWN FAILED");
	let sender1 = Sender::<usize>::new(pid);
	let sender2 = Sender::<usize>::new(pid);
}
