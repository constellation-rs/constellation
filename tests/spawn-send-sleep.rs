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
//=           "hi\nho\n",
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
//=           "hi\nho\n",
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
				println!("{}", receiver.recv().block().unwrap());
				std::thread::sleep(std::time::Duration::from_millis(1000));
				println!("{}", receiver.recv().block().unwrap());
			}),
		)
		.block()
		.expect("spawn() failed");
		let sender = Sender::<String>::new(pid);
		sender.send(String::from("hi")).block();
		std::thread::sleep(std::time::Duration::from_millis(100));
		sender.send(String::from("ho")).block();
	}
}
