//= {
//=   "output": {
//=     "1": [
//=       "",
//=       true
//=     ],
//=     "2": [
//=       "",
//=       true
//=     ]
//=   },
//=   "children": [],
//=   "exit": {
//=     "Left": 0
//=   }
//= }

#![deny(warnings, deprecated)]
extern crate deploy;
use deploy::*;
use std::{thread, time};

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	thread::sleep(time::Duration::new(0, 100_000_000));
}
