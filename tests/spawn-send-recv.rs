//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "1234567890\n1234567890\n",
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

fn main() {
	init(Resources {
		mem: 20 * Mem::MIB,
		..Resources::default()
	});
	for _ in 0..2 {
		let pid = spawn(
			Resources {
				mem: 20 * Mem::MIB,
				..Resources::default()
			},
			FnOnce!(|parent| {
				let receiver = Receiver::<String>::new(parent);
				let sender = Sender::<usize>::new(parent);
				println!("{}", receiver.recv().block().unwrap());
				sender.send(1_234_567_890).block();
			}),
		)
		.block()
		.expect("spawn() failed to allocate process");
		let sender = Sender::<String>::new(pid);
		let receiver = Receiver::<usize>::new(pid);
		sender.send(String::from("hi")).block();
		println!("{}", receiver.recv().block().unwrap());
	}
}
