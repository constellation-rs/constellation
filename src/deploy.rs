use either::Either;
use std::{
	collections::HashSet, ffi, fs, io::{self, Read, Write}, mem::MaybeUninit, net, path, process
};

use constellation_internal::{
	abort_on_unwind_1, map_bincode_err, msg::{bincode_serialize_into, BridgeRequest}, BufferedStream, DeployInputEvent, DeployOutputEvent, ExitStatus, Format, Formatter, Pid, StyleSupport, TrySpawnError
};

/// Unstable
#[doc(hidden)]
pub fn deploy(
	bridge_address: net::SocketAddr, path: &path::PathBuf, format: Format,
	args: Vec<ffi::OsString>, vars: Vec<(ffi::OsString, ffi::OsString)>,
) {
	let stream = net::TcpStream::connect(&bridge_address)
		.unwrap_or_else(|e| panic!("Could not connect to {:?}: {:?}", bridge_address, e));
	let (mut stream_read, mut stream_write) =
		(BufferedStream::new(&stream), BufferedStream::new(&stream));
	#[cfg(feature = "distribute_binaries")]
	let binary =
		fs::File::open(path).unwrap_or_else(|e| panic!("Couldn't open file {:?}: {:?}", path, e));
	#[cfg(not(feature = "distribute_binaries"))]
	let binary = std::marker::PhantomData::<fs::File>;
	let request = BridgeRequest {
		resources: None,
		args,
		vars,
		arg: vec![],
		binary,
	};
	bincode_serialize_into(&mut stream_write.write(), &request)
		.map_err(map_bincode_err)
		.unwrap_or_else(|e| panic!("Couldn't communicate with bridge: {:?}", e));
	let pid: Result<Pid, TrySpawnError> = bincode::deserialize_from(&mut stream_read)
		.map_err(map_bincode_err)
		.unwrap_or_else(|e| panic!("Couldn't communicate with bridge: {:?}", e));
	let pid = pid.unwrap_or_else(|e| panic!("Deploy failed due to {}", e)); // TODO get resources from bridge
	crossbeam::scope(|scope| {
		let _ = scope.spawn(abort_on_unwind_1(|_scope| {
			let mut stdin = io::stdin();
			loop {
				let mut buf = MaybeUninit::<[u8; 1024]>::uninit();
				#[cfg(feature = "nightly")]
				unsafe {
					stdin.initializer().initialize(&mut *buf.as_mut_ptr());
				}
				let n = stdin.read(unsafe { &mut *buf.as_mut_ptr() }).unwrap();
				bincode::serialize_into(
					&mut stream_write.write(),
					&DeployInputEvent::Input(pid, 0, unsafe { &(&*buf.as_ptr())[..n] }.to_owned()),
				)
				.unwrap();
				if n == 0 {
					break;
				}
			}
		}));
		let mut exit_code = ExitStatus::Success;
		let mut ref_count = 1;
		let mut pids = HashSet::new();
		let _ = pids.insert(pid);
		let (stdout, stderr) = (io::stdout(), io::stderr());
		let mut formatter = if let Format::Human = format {
			Either::Left(Formatter::new(
				pid,
				if atty::is(atty::Stream::Stderr) {
					StyleSupport::EightBit
				} else {
					StyleSupport::None
				},
				stdout.lock(),
				stderr.lock(),
			))
		} else {
			Either::Right(stdout.lock())
		};
		loop {
			let event: DeployOutputEvent = bincode::deserialize_from(&mut stream_read)
				.map_err(map_bincode_err)
				.expect("Bridge died");
			match formatter {
				Either::Left(ref mut formatter) => formatter.write(&event),
				Either::Right(ref mut stdout) => {
					serde_json::to_writer(&mut *stdout, &event).unwrap();
					stdout.write_all(b"\n").unwrap()
				}
			}
			match event {
				DeployOutputEvent::Spawn(pid, new_pid) => {
					assert_ne!(pid, new_pid);
					assert!(pids.contains(&pid));
					ref_count += 1;
					let x = pids.insert(new_pid);
					assert!(x);
				}
				DeployOutputEvent::Output(pid, _fd, _output) => {
					assert!(pids.contains(&pid));
				}
				DeployOutputEvent::Exit(pid, exit_code_) => {
					exit_code += exit_code_;
					ref_count -= 1;
					let x = pids.remove(&pid);
					assert!(x);
					// printer.eprint(format_args!("   {} {:?}\nremaining: {}\n", ansi_term::Style::new().bold().paint("exited:"), exit_code_, std::slice::SliceConcatExt::join(&*pids.iter().map(|pid|pretty_pid(pid,false).to_string()).collect::<Vec<_>>(), ",")));
				}
			}
			if ref_count == 0 {
				break;
			}
		}
		process::exit(exit_code.into());
	})
	.unwrap();
}
