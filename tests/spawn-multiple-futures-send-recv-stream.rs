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
//=       "exit": "Success"
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
//=       "exit": "Success"
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
//=       "exit": "Success"
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
//=       "exit": "Success"
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
//=       "exit": "Success"
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
//=       "exit": "Success"
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
//=       "exit": "Success"
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
//=       "exit": "Success"
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
//=       "exit": "Success"
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
//=       "exit": "Success"
//=     }
//=   ],
//=   "exit": "Success"
//= }

use constellation::*;
use futures::{future::FutureExt, sink::SinkExt, stream::StreamExt};
use serde_closure::FnOnce;

fn main() {
	init(Resources {
		mem: 20 * Mem::MIB,
		..Resources::default()
	});
	let x = futures::executor::block_on(futures::future::join_all((0..10).map(|i| {
		let pid = spawn(
			Resources {
				mem: 20 * Mem::MIB,
				..Resources::default()
			},
			FnOnce!(move |parent| {
				println!("hi {}", i);
				let (receiver, sender) = (
					Receiver::<Option<String>>::new(parent),
					Sender::<Option<String>>::new(parent),
				);
				futures::executor::block_on(
					receiver.forward(sender.sink_map_err(|_| unreachable!())),
				)
				.unwrap();
				println!("done {}", i);
			}),
		)
		.block()
		.expect("spawn() failed to allocate process");
		let (sender, receiver) = (
			Sender::<Option<String>>::new(pid),
			Receiver::<Option<String>>::new(pid),
		);
		futures::future::join(
			futures::stream::iter(
				vec![
					String::from("abc"),
					String::from("def"),
					String::from("ghi"),
					String::from("jkl"),
					String::from("mno"),
				]
				.into_iter()
				.map(Ok),
			)
			.forward(sender.sink_map_err(|_| unreachable!())),
			receiver.fold(String::new(), |acc, x| {
				futures::future::ready(acc + &x.unwrap())
			}),
		)
		.map(|(_, res)| res)
	})));
	println!("{:?}", x);
}
