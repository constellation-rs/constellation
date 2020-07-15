//= {
//=   "output": {
//=     "2": [
//=       "thread 'main' panicked at 'Sender::<.*>::new\\(\\) called for pid [a-z0-9]{7} when a Sender to this pid already exists', .*:[0-9]+:[0-9]+\n",
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
//=         "1": [
//=           "",
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
//=   "exit": {
//=     "Error": {
//=       "Unix": {
//=         "Signal": "SIGABRT"
//=       }
//=     }
//=   }
//= }

use constellation::*;
use std::{panic, process, thread};

fn main() {
	init(Resources {
		mem: 20 * Mem::MIB,
		..Resources::default()
	});
	panic::set_hook(Box::new(|info| {
		eprintln!(
			"thread '{}' {}",
			thread::current().name().unwrap_or("<unnamed>"),
			info
		);
		let err = unsafe {
			nix::libc::setrlimit(
				nix::libc::RLIMIT_CORE,
				&nix::libc::rlimit {
					rlim_cur: 0,
					rlim_max: 0,
				},
			)
		};
		assert_eq!(err, 0);
		process::abort()
	}));
	let pid = spawn(
		Resources {
			mem: 20 * Mem::MIB,
			..Resources::default()
		},
		FnOnce!(|_parent| ()),
	)
	.block()
	.expect("spawn() failed to allocate process");
	let _sender1 = Sender::<usize>::new(pid);
	let _sender2 = Sender::<usize>::new(pid);
}
