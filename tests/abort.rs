//= {
//=   "output": {
//=     "2": [
//=       "thread 'main' panicked at 'qwertyuiop', tests/.*\\.rs:[0-9]+:[0-9]+\n",
//=       true
//=     ],
//=     "1": [
//=       "",
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

use constellation::*;
use std::{panic, process, thread};

fn main() {
	init(Resources {
		mem: 20 * 1024 * 1024,
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
	panic!("qwertyuiop");
}
