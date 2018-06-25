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

extern crate deploy;
use deploy::*;
fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
}
