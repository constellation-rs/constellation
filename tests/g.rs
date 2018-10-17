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
//=   "exit": "Success"
//= }

#![deny(warnings, deprecated)]
extern crate constellation;
use constellation::*;

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
}
