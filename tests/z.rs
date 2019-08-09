//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "Resources \\{ mem: 20971520, cpu: 4096 \\}\n",
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
//=           "hi Resources \\{ mem: 20971520, cpu: 65 \\}\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": "Success"
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi Resources \\{ mem: 20971521, cpu: 65 \\}\n",
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
//=           "hi Resources \\{ mem: 20971522, cpu: 65 \\}\n",
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
//=           "hi Resources \\{ mem: 20971523, cpu: 65 \\}\n",
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
	println!("{:?}", resources());
	for i in 0..4 {
		let _pid = spawn(
			Resources {
				mem: 20 * 1024 * 1024 + i,
				cpu: 65536 / 1000,
			},
			FnOnce!([i] move |_parent| {
				assert_eq!(resources().mem, 20 * 1024 * 1024 + i);
				println!("hi {:?}", resources());
			}),
		)
		.block()
		.expect("spawn() failed");
	}
}
