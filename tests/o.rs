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
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     }
//=   ],
//=   "exit": "Success"
//= }

#![deny(warnings, deprecated)]
#[macro_use]
extern crate serde_closure;
extern crate constellation;
use constellation::*;

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	for _ in 0..2 {
		let pid = spawn(
			Resources {
				mem: 20 * 1024 * 1024,
				..Resources::default()
			},
			FnOnce!(|parent| {
				let receiver = Receiver::<String>::new(parent);
				println!("{}", receiver.recv().unwrap());
			}),
		)
		.expect("SPAWN FAILED");
		let sender = Sender::<String>::new(pid);
		sender.send(String::from("hi"));
	}
}
