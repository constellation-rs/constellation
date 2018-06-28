//= {
//=   "output": {
//=     "2": [
//=       "thread 'main' panicked at 'Sender::<usize>::new\\(\\) called for pid [a-z0-9]{7} when a Sender to this pid already exists', deploy/src/lib\\.rs:[0-9]+:[0-9]+\n",
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
//=       "exit": {
//=         "Left": 0
//=       }
//=     }
//=   ],
//=   "exit": {
//=     "Right": "SIGABRT"
//=   }
//= }

#![deny(warnings, deprecated)]
extern crate deploy;
extern crate nix;
use deploy::*;
use std::{panic, process, thread};

fn sub<T>(_parent: Pid, _arg: T) {}

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	panic::set_hook(Box::new(|info| {
		eprintln!("thread '{}' {}", thread::current().name().unwrap(), info);
		let err = unsafe {
			nix::libc::setrlimit64(
				nix::libc::RLIMIT_CORE,
				&nix::libc::rlimit64 {
					rlim_cur: 0,
					rlim_max: 0,
				},
			)
		};
		assert_eq!(err, 0);
		process::abort()
	}));
	let pid = spawn(
		sub,
		(),
		Resources {
			mem: 20 * 1024 * 1024,
			..Resources::default()
		},
	).expect("SPAWN FAILED");
	let _sender1 = Sender::<usize>::new(pid);
	let _sender2 = Sender::<usize>::new(pid);
}
