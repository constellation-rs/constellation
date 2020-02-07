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
//=     "Error": {
//=       "Unix": {
//=         "Status": 1
//=       }
//=     }
//=   }
//= }

use constellation::*;
use std::{process, thread, time};

fn main() {
	init(Resources {
		mem: 20 * Mem::MIB,
		..Resources::default()
	});
	thread::sleep(time::Duration::new(0, 100_000_000));
	process::exit(1);
}
