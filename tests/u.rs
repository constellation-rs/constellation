//= {
//=   "output": {
//=     "1": [
//=       "",
//=       true
//=     ],
//=     "2": [
//=       "thread 'main' panicked at 'Sender::<usize>::new\\(\\) called with process's own pid\\. A process cannot create a channel to itself\\.', src/lib\\.rs:[0-9]+:[0-9]+\n",
//=       true
//=     ]
//=   },
//=   "children": [],
//=   "exit": {
//=     "Error": {
//=       "Unix": {
//=         "Signal": "SIGABRT"
//=       }
//=     }
//=   }
//= }

#![deny(warnings, deprecated)]
extern crate constellation;
extern crate nix;
use constellation::*;
use std::{panic, process, thread};

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
		..Resources::default()
	});
	panic::set_hook(Box::new(|info| {
		eprintln!("thread '{}' {}", thread::current().name().unwrap(), info);
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
	let pid = pid();
	let _sender = Sender::<usize>::new(pid);
}
