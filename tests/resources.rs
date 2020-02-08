//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "Resources \\{ mem: Mem\\(20971520\\), cpu: Cpu\\(4096\\) \\}\n",
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
//=           "hi Resources \\{ mem: Mem\\(20971520\\), cpu: Cpu\\(65\\) \\}\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi Resources \\{ mem: Mem\\(20971521\\), cpu: Cpu\\(65\\) \\}\n",
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
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi Resources \\{ mem: Mem\\(20971522\\), cpu: Cpu\\(65\\) \\}\n",
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
//=           "hi Resources \\{ mem: Mem\\(20971523\\), cpu: Cpu\\(65\\) \\}\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     }
//=   ],
//=   "exit": "Success"
//= }

#![allow(clippy::needless_update)]

use constellation::*;

fn main() {
	init(Resources {
		mem: 20 * Mem::MIB,
		..Resources::default()
	});
	println!("{:?}", resources());
	for i in 0..4 {
		let _pid = spawn(
			Resources {
				mem: 20 * Mem::MIB + i * Mem::B,
				cpu: Cpu::CORE / 1000,
				..Resources::default()
			},
			FnOnce!(move |_parent| {
				assert_eq!(resources().mem, 20 * Mem::MIB + i * Mem::B);
				println!("hi {:?}", resources());
			}),
		)
		.block()
		.expect("spawn() failed to allocate process");
	}
}
