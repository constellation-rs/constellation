//= {
//=   "output": {
//=     "1": [
//=       "1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\n1234567890\nho\nho\nho\nho\nho\nho\nho\nho\nho\nho\n",
//=       true
//=     ],
//=     "2": [
//=       "",
//=       true
//=     ]
//=   },
//=   "children": [
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi\n987654321\n",
//=           true
//=         ],
//=         "2": [
//=           "",
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
use std::mem;

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	let pids = (0..10)
		.map(|_| {
			spawn(
				Resources {
					mem: 20 * 1024 * 1024,
					..Resources::default()
				},
				FnOnce!(|parent| {
					let receiver = Receiver::<String>::new(parent);
					let sender = Sender::<usize>::new(parent);
					println!("{}", receiver.brecv().unwrap());
					sender.bsend(1_234_567_890);
					mem::drop((receiver, sender));
					let receiver = Receiver::<usize>::new(parent);
					let sender = Sender::<String>::new(parent);
					sender.bsend(String::from("ho"));
					println!("{}", receiver.brecv().unwrap());
				}),
			)
			.expect("SPAWN FAILED")
		})
		.collect::<Vec<_>>();
	let channels = pids
		.iter()
		.map(|&pid| (Sender::<String>::new(pid), Receiver::<usize>::new(pid)))
		.collect::<Vec<_>>();
	for &(ref sender, ref _receiver) in channels.iter() {
		sender.bsend(String::from("hi"));
	}
	for &(ref _sender, ref receiver) in channels.iter() {
		println!("{}", receiver.brecv().unwrap());
	}
	mem::drop(channels);
	let channels = pids
		.iter()
		.map(|&pid| (Sender::<usize>::new(pid), Receiver::<String>::new(pid)))
		.collect::<Vec<_>>();
	for &(ref _sender, ref receiver) in channels.iter() {
		println!("{}", receiver.brecv().unwrap());
	}
	for &(ref sender, ref _receiver) in channels.iter() {
		sender.bsend(987_654_321);
	}
}
