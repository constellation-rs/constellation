<p align="center">
    <img alt="Constellation" src="https://raw.githubusercontent.com/alecmocatta/constellation/master/logo.svg?sanitize=true" width="550" />
</p>

<p align="center">
    A project to make Rust the cutting edge of distributed computing.
</p>

<p align="center">
    <a href="https://crates.io/crates/constellation-rs"><img src="https://img.shields.io/crates/v/deploy.svg?maxAge=86400" alt="Crates.io" /></a>
    <a href="LICENSE.txt"><img src="https://img.shields.io/crates/l/deploy.svg?maxAge=2592000" alt="Apache-2.0 licensed" /></a>
    <a href="https://dev.azure.com/alecmocatta/constellation/_build/latest?branchName=master"><img src="https://dev.azure.com/alecmocatta/constellation/_apis/build/status/tests?branchName=master" alt="Build Status" /></a>
</p>

<p align="center">
    <a href="https://docs.rs/constellation-rs/0.1.4">Docs</a>
</p>

Constellation is a framework for Rust (nightly) that aides in the writing, debugging and deployment of distributed programs. It draws heavily from [Erlang/OTP](https://en.wikipedia.org/wiki/Erlang_(programming_language)), [MPI](https://en.wikipedia.org/wiki/Message_Passing_Interface), and [CSP](https://en.wikipedia.org/wiki/Communicating_sequential_processes); and leverages the Rust ecosystem where it can including [serde](https://serde.rs/) + [bincode](https://github.com/TyOverby/bincode) for network serialization, [futures-rs](https://github.com/rust-lang-nursery/futures-rs) for integration with [tokio](https://tokio.rs/), and the semantics of [crossbeam](https://github.com/crossbeam-rs/crossbeam-channel)'s channels and `select()` for channels over TCP.

Most users will leverage Constellation through higher-level libraries, such as:

 * **[Amadeus](https://github.com/alecmocatta/amadeus)**: Harmonious distributed data analysis in Rust. Inspired by [Rayon](https://github.com/rayon-rs/rayon), it provides a distributed process pool and built-in data science tools to leverage it.
 * With more in the pipeline!

For leveraging Constellation directly, read on.

## Constellation framework

* Constellation is a framework that's initialised with a call to [`init()`](https://docs.rs/constellation-rs/0.1.4/constellation/fn.init.html) at the beginning of your program.
* You can [`spawn(closure)`](https://docs.rs/constellation-rs/0.1.4/constellation/fn.spawn.html) new processes, which run `closure`.
* You can communicate between processes by creating channels with [`Sender::new(remote_pid)`](https://docs.rs/constellation-rs/0.1.4/constellation/struct.Sender.html#method.new) and [`Receiver::new(remote_pid)`](https://docs.rs/constellation-rs/0.1.4/constellation/struct.Receiver.html#method.new).
* Channels can be used with the blocking [`sender.send()`](https://docs.rs/constellation-rs/0.1.4/constellation/struct.Sender.html#method.send) and [`receiver.recv()`](https://docs.rs/constellation-rs/0.1.4/constellation/struct.Receiver.html#method.recv), with the more powerful [`select()`](https://docs.rs/constellation-rs/0.1.4/constellation/fn.select.html), with Rust's [futures-rs](https://github.com/rust-lang-nursery/futures-rs) combinators and with [async/await syntax](https://github.com/rust-lang/rfcs/blob/master/text/2394-async_await.md).

Here's an example program leveraging [fork-join parallelism](https://en.wikipedia.org/wiki/Fork–join_model) to distribute the task of finding "low" SHA1 hashes:

<details>
<summary>Click to show Cargo.toml.</summary>

```toml
[dependencies]

# The core APIs, including init(), spawn(), Sender, Receiver and select().
# Always required when using Constellation.
constellation-rs = "0.1"

# Support for FnOnce!(), FnMut!() and Fn!() macros to create Serde serializable
# closures. Required to pass a closure to spawn().
serde_closure = "0.1"

# Miscellaneous dependencies for this specific example.
hex = "0.3"
rand = "0.5"
sha1 = "0.6"
```

</details>
<p></p>

```rust
#[macro_use]
extern crate serde_closure;
extern crate constellation;
extern crate hex;
extern crate rand;
extern crate sha1;

use constellation::*;
use rand::{distributions::Alphanumeric, Rng};
use sha1::Sha1;
use std::{env, iter, time};

fn main() {
    init(Resources::default());

    // Accept the number of processes at the command line, defaulting to 10
    let processes = env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<usize>().ok())
        .unwrap_or(10);

    let processes = (0..processes).map(|i| {

        // Spawn the following FnOnce closure in a new process
        let child = spawn(
            // Use the default resource limits, which are enough for this example
            Resources::default(),
            // Make this closure serializable by wrapping with serde_closure's
            // FnOnce!() macro, which requires explicitly listing captured variables.
            FnOnce!([i] move |parent| {
                println!("process {}: commencing hashing", i);

                let mut rng = rand::thread_rng();

                // To record the lowest hash value seen
                let mut lowest: Option<(String,[u8;20])> = None;

                // Loop for ten seconds
                let start = time::Instant::now();
                while start.elapsed() < time::Duration::new(10,0) {
                    // Generate a random 7 character string
                    let string: String = iter::repeat(()).map(|()| rng.sample(Alphanumeric)).take(7).collect();

                    // Hash the string
                    let hash = Sha1::from(&string).digest().bytes();

                    // Update our record of the lowest hash value seen
                    if lowest.is_none() || lowest.as_ref().unwrap().1 >= hash {
                        lowest = Some((string,hash));
                    }
                }

                let lowest = lowest.unwrap();
                println!("process {}: lowest hash was {} from string \"{}\"", i, hex::encode(lowest.1), lowest.0);

                // Create a `Sender` half of a channel to our parent
                let sender = Sender::<(String,[u8;20])>::new(parent);

                // Send our record along the channel to our parent
                sender.send(lowest);
            }),
        ).expect("Unable to allocate process!");

        // Create a `Receiver` half of a channel to the newly-spawned child
        Receiver::<(String, [u8; 20])>::new(child)

    }).collect::<Vec<_>>();

    // `processes` is now a Vec of `Receiver`s

    let result = processes
        .into_iter()
        // Receive a record from each `Receiver`
        .map(|receiver| receiver.recv().unwrap())
        // Take the record with the lowest hash
        .min_by_key(|&(_, hash)| hash)
        .unwrap();

    println!(
        "overall lowest hash was {} from string \"{}\"",
        hex::encode(result.1),
        result.0
    );
}
```

## ***TODO***

![Screencap of constellation being used](http://deploy-rs.s3-website-us-east-1.amazonaws.com/deploy.gif)

## Running distributed

There are two components to Constellation:
 * a library of functions that enable you to `spawn()` processes, and `send()` and `recv()` between them
 * for when you want to run across multiple servers, a distributed execution fabric, plus the `deploy` command added to cargo to deploy programs to it.

Both output to the command line as show above – the only difference is the latter has been forwarded across the network.

Constellation is still nascent – development and testing is ongoing to bring support to Windows (currently it's Linux and macOS only) and reach a level of maturity sufficient for production use.

The primary efforts right now are on testing, documentation, refining the API (specifically error messages and usability of `select()`), and porting to macOS and Windows.

## Features
Constellation takes care of:
 * `spawn()` to distribute processes with defined memory and CPU resource requirements to servers with available resources
 * Best-effort enforcement of those memory and resource requirements to avoid buggy/greedy processes starving others
 * Channels between processes over TCP, with automatic setup and teardown
 * Asynchronous (de)serialisation of values sent/received over channels (leveraging [`serde`](https://crates.io/crates/serde) and [`libfringe`](https://github.com/edef1c/libfringe))
 * `select()` to select over receivability/sendability of channels
 * Integration with [`futures-rs`](https://github.com/rust-lang-nursery/futures-rs), thereby compatible with [`tokio`](https://github.com/tokio-rs/tokio)
 * Powered by a background thread running an efficient edge-triggered epoll loop
 * Ensuring data is sent and acked before process exit to avoid connection resets and lost data (leveraging [`atexit`](http://pubs.opengroup.org/onlinepubs/000095399/functions/atexit.html) and [`TIOCOUTQ`](https://blog.netherlabs.nl/articles/2009/01/18/the-ultimate-so_linger-page-or-why-is-my-tcp-not-reliable))
 * Addressing: all channels are between cluster-wide `Pid`s, rather than `(ip,port)`s
 * Performant: designed to bring minimal overhead above the underlying OS

## What's it for
Constellation makes it easier to write a distributed program. Akin to MPI, it abstracts away Berkeley sockets, letting you focus on the business logic rather than the addressing, connecting, multiplexing, asynchrony, eventing and teardown. Unlike MPI, it has a sane, modern, concise interface, that handles (de)serialisation using [`serde`](https://crates.io/crates/serde), offers powerful async building blocks like `select()`, and integrates with frameworks like [`tokio`](https://tokio.rs/).

## How it works
There are two execution modes: running normally with `cargo run` and deploying to a cluster with `cargo deploy`. We'll discuss the first, and then cover what differs in the second.

#### Monitor process
Every process has a **monitor process** that captures the process's output, and calls `waitpid` on it to capture the exit status (be it exit code or signal). This is set up by forking upon process initialisation, parent being the monitor and the child going on to run the user's program. It captures the output by replacing file descriptors 0,1,2 (which correspond to stdin, stdout and stderr) with pipes, such that when the user's process writes to e.g. fd 1, it's writing to a pipe that the monitor process then reads from and forwards to the **bridge**.

#### Bridge
The **bridge** is what collects the output from the various **monitor processes** and outputs it formatted at the terminal. It is started inside `init()`, with the process forking such that the parent becomes the bridge, while the child goes on to run the user's program.

### Spawning
`spawn()` takes a function, an argument, and resource constraints, and spawns a new process with them. This works by invoking a clean copy of the current binary with `execve("/proc/self/exe",argv,envp)`, which, in its invocation of `init()`, acts slightly differently: it connects back to the preexisting bridge, and rather than returning control flow back up, it invokes the specified user function with the user argument, before exiting normally. The function pointer is adjusted relative to a fixed base in the text section.

#### Channels
Communication happens by creating `Sender<T>`s and `Receiver<T>`s. Creation takes a `Pid`, and does quite a bit of bookkeeping behind the scenes to ensure that:
 * Duplex TCP connections are created and tore down correctly and opportunely to back the simplex channels created by the user.
 * The resource consumption in the OS of TCP connections is proportional to the number of channels held by the user.
 * `Pid`s are unique.
 * Each process has a single port (bound ephemerally at initialisation to avoid starvation or failure) that all channel-backing TCP connections are to or from.
 * (De)serialisation can occur asynchronously, i.e. to avoid having to allocate unbounded memory to hold the result of serde's serialisation if the socket is not ready to be written to, leverage coroutines courtesy of [`libfringe`](https://github.com/edef1c/libfringe).
 * The type of a channel's message can be changed by dropping and recreating it.

### Running distributed
There are four main differences when running on a cluster:

#### Constellation Node
Listens on a configurable address, receiving binaries and executing them.

#### Constellation Master
Takes addresses and resources of the zero or more other **constellation** instances as input, as well as what processes to start automatically – this will almost always be the **bridge**.

It listens on a configurable address for binaries with resource requirements to deploy – but almost always it only makes sense for the **bridge** to be giving it these binaries.

#### Bridge
Rather than being invoked by a fork inside the user process, it is started automatically at constellation master-initialisation time. It listens on a configurable address for `cargo deploy`ments, at which point it runs the binary with special env vars that trigger `init()` to print resource requirements of the initial process and exit, before sending the binary with the determined resource requirements to the **constellation master**. Upon being successfully allocated, it is executed by a **constellation** instance. Inside `init()`, it connects back to the **bridge**, which dutifully forwards its output to `cargo deploy`.

#### `cargo deploy`
This is a command [added to](https://doc.rust-lang.org/book/second-edition/ch14-05-extending-cargo.html#extending-cargo-with-custom-commands) cargo that under the hood invokes `cargo run`, except that rather than the resulting binary being run locally, it is sent off to the **bridge**. The **bridge** then sends back any output, which is output formatted at the terminal.

## How to use it
```toml
[dependencies]
constellation-rs = "0.1"
```
```
extern crate constellation;
use constellation::*;
fn main() {
    init(Resources::default());
    println!("Hello, world!");
}
```
```
$ cargo run
3fecd01:
    Hello, world!
    exited: 0
```
Or, to run distributed:
Machine 2:
```
cargo install constellation-rs
constellation 10.0.0.2:9999
```
Machine 3:
```
cargo install constellation-rs
constellation 10.0.0.3:9999
```
Machine 1:
```
cargo install constellation-rs
constellation master 10.0.0.1:9999 400GiB 34 bridge 10.0.0.1:8888 \
              10.0.0.2:9999 400GiB 34 \
              10.0.0.3:9999 400GiB 34
```
Your laptop:
```
cargo install constellation-rs
cargo deploy 10.0.0.1:8888 --release
833d3de:
    Hello, world!
    exited: 0
```

#### Requirements
Rust: nightly.

Linux: kernel >= 3.9; `/proc` filesystem.

macOS: Tested >= 10.10, may work on older versions too.

Arch: x86-64 (x86, aarch64 and or1k may work but are untested).

Please file an issue if you experience any other requirements.

## API

[see Rust doc](https://docs.rs/constellation-rs/0.1.4)

## Testing

[see TESTING.md](TESTING.md)

## Why?

Constellation forms the basis of a large-scale data processing project I'm working on. I decided to start polishing it and publish it as open source on the off chance it might be interesting or even useful to anyone else!

## License
Licensed under Apache License, Version 2.0, ([LICENSE.txt](LICENSE.txt) or http://www.apache.org/licenses/LICENSE-2.0).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be licensed as above, without any additional terms or conditions.

<!--
[![Gitter](https://img.shields.io/gitter/room/constellation-rs/constellation.js.svg)](https://gitter.im/alecmocatta/constellation)
[Tutorial](TUTORIAL.md) |
[Chat](https://gitter.im/alecmocatta/constellation)
-->
