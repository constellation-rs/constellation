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

use constellation::*;
use serde_closure::FnOnce;

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
				println!("{}", receiver.recv().block().unwrap());
			}),
		)
		.block()
		.expect("spawn() failed");
		let sender = Sender::<String>::new(pid);
		sender.send(String::from("hi")).block();
	}
}
