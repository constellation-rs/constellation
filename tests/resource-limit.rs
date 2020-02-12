//= {"output":{"2":["",true],"1":["",true]},"children":[{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"},{"output":{"2":["",true],"1":["",true]},"children":[],"exit":"Success"}],"exit":"Success"}

use constellation::*;
use std::env;

fn main() {
	init(Resources {
		mem: 2 * Mem::GIB,
		..Resources::default()
	});
	let fabric = env::var("CONSTELLATION") == Ok(String::from("fabric"));
	if fabric {
		try_spawn(
			Resources {
				mem: 2 * Mem::GIB,
				..Resources::default()
			},
			FnOnce!(|_parent| ()),
		)
		.block()
		.err()
		.expect("spawn() should have failed");
	}
	for _ in 0..7 {
		let pid = spawn(
			Resources {
				mem: Mem::GIB,
				..Resources::default()
			},
			FnOnce!(|parent| Receiver::<()>::new(parent).recv().block().unwrap()),
		)
		.block()
		.expect("spawn() failed to allocate process");
		let sender1 = Sender::<()>::new(pid);
		if fabric {
			try_spawn(
				Resources {
					mem: Mem::GIB,
					..Resources::default()
				},
				FnOnce!(|_parent| ()),
			)
			.block()
			.err()
			.expect("spawn() should have failed");
		}
		for _ in 0..6 {
			let pid = spawn(
				Resources {
					mem: Mem::GIB / 2,
					..Resources::default()
				},
				FnOnce!(|parent| Receiver::<()>::new(parent).recv().block().unwrap()),
			)
			.block()
			.expect("spawn() failed to allocate process");
			let sender2 = Sender::<()>::new(pid);
			if fabric {
				try_spawn(
					Resources {
						mem: Mem::GIB / 2,
						..Resources::default()
					},
					FnOnce!(|_parent| ()),
				)
				.block()
				.err()
				.expect("spawn() should have failed");
				try_spawn(
					Resources {
						mem: Mem::B,
						..Resources::default()
					},
					FnOnce!(|_parent| ()),
				)
				.block()
				.err()
				.expect("spawn() should have failed");
			}
			sender2.send(()).block();
		}
		sender1.send(()).block();
	}
}
