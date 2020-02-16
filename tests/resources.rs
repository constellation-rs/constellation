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
use palaver::file::FdIter;
use std::{collections::HashSet, env};

fn main() {
	let fds = FdIter::new().unwrap().collect::<HashSet<_>>();
	init(Resources {
		mem: 20 * Mem::MIB,
		..Resources::default()
	});
	let binary_fd = cfg!(feature = "distribute_binaries") as i32;
	let expected_fds = match (
		env::var("CONSTELLATION").as_ref().map(|x| &**x),
		env::var("CONSTELLATION_RECCE").as_ref().map(|x| &**x),
		env::var("CONSTELLATION_RESOURCES").as_ref().map(|x| &**x),
	) {
		(Ok("fabric"), Ok("1"), _) => 0..4 + binary_fd, // bridge recce: output, (binary)
		(Ok("fabric"), _, _) => 0..5 + binary_fd,       // fabric: listener, arg, (binary)
		(_, _, Ok(_)) => 0..5,                          // native sub: listener, arg
		(_, _, _) => 0..3,                              // native top
	};
	assert_eq!(fds, expected_fds.collect());
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
