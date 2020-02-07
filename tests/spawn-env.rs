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
use std::env;

fn main() {
	init(Resources {
		mem: 20 * Mem::MIB,
		..Resources::default()
	});
	let env = (
		env::args().collect::<Vec<_>>(),
		env::vars()
			.filter(|&(ref key, _)| key != "CONSTELLATION" && key != "CONSTELLATION_RESOURCES")
			.collect::<Vec<_>>(),
	);
	for _ in 0..2 {
		let env = env.clone();
		let _pid = spawn(
			Resources {
				mem: 20 * Mem::MIB,
				..Resources::default()
			},
			FnOnce!(|_parent| {
				let env2 = (
					env::args().collect::<Vec<_>>(),
					env::vars()
						.filter(|&(ref key, _)| {
							key != "CONSTELLATION" && key != "CONSTELLATION_RESOURCES"
						})
						.collect::<Vec<_>>(),
				);
				assert_eq!(env, env2);
			}),
		)
		.block()
		.expect("spawn() failed to allocate process");
	}
}
