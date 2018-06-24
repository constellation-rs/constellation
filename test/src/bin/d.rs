//= {
//=   "output": {
//=     "2": [
//=       "thread 'main' panicked at 'qwertyuiop', test/src/bin/d\\.rs:28:2\n",
//=       true
//=     ],
//=     "1": [
//=       "",
//=       true
//=     ]
//=   },
//=   "children": [],
//=   "exit": {
//=     "Right": "SIGABRT"
//=   }
//= }

extern crate deploy;
use std::{thread,time,panic,process};
use deploy::*;
fn main() {
	init(Resources{mem:20*1024*1024,..Resources::default()});
	panic::set_hook(Box::new(|info|{
		eprintln!("thread '{}' {}", thread::current().name().unwrap(), info);
		process::abort()
	}));
	thread::sleep(time::Duration::new(0,100_000_000));
	panic!("qwertyuiop");
}
