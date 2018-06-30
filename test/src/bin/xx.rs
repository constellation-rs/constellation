//= {
//=   "output": {
//=     "2": [
//=       "",
//=       true
//=     ],
//=     "1": [
//=       "\\[\"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\", \"abcdefghijklmno\"\\]\n",
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
//=           "hi 4\ndone 4\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi 6\ndone 6\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi 7\ndone 7\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi 9\ndone 9\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi 1\ndone 1\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi 5\ndone 5\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi 3\ndone 3\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi 0\ndone 0\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "1": [
//=           "hi 2\ndone 2\n",
//=           true
//=         ],
//=         "2": [
//=           "",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     },
//=     {
//=       "output": {
//=         "2": [
//=           "",
//=           true
//=         ],
//=         "1": [
//=           "hi 8\ndone 8\n",
//=           true
//=         ]
//=       },
//=       "children": [],
//=       "exit": {
//=         "Left": 0
//=       }
//=     }
//=   ],
//=   "exit": {
//=     "Left": 0
//=   }
//= }

#![deny(warnings, deprecated)]
extern crate deploy;
use deploy::*;

fn sub(parent: Pid, arg: u32) {
	println!("hi {}", arg);
	let receiver = Receiver::<Option<String>>::new(parent);
	let sender = Sender::<Option<String>>::new(parent);
	loop {
		let x = receiver.recv().unwrap();
		let end = x.is_none();
		sender.send(x).unwrap();
		if end {
			break;
		}
	}
	println!("done {}", arg);
}

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	let workers = (0..10)
		.map(|i| {
			let pid = spawn(
				sub,
				i,
				Resources {
					mem: 20 * 1024 * 1024,
					..Resources::default()
				},
			).expect("SPAWN FAILED");
			(
				Sender::<Option<String>>::new(pid),
				Receiver::<Option<String>>::new(pid),
			)
		})
		.collect::<Vec<_>>();
	let xx = vec![
		String::from("abc"),
		String::from("def"),
		String::from("ghi"),
		String::from("jkl"),
		String::from("mno"),
	];
	for &(ref sender, _) in &workers {
		for x in &xx {
			sender.send(Some(x.clone())).unwrap();
		}
		sender.send(None).unwrap();
	}
	let x = workers
		.iter()
		.map(|&(_, ref receiver)| {
			let x = xx
				.iter()
				.map(|x| {
					let y = receiver.recv().unwrap();
					assert_eq!(Some(x.clone()), y);
					y.unwrap()
				})
				.collect::<Vec<_>>()
				.join("");
			let y = receiver.recv().unwrap();
			assert_eq!(None, y);
			x
		})
		.collect::<Vec<_>>();
	println!("{:?}", x);
}
