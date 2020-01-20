<p align="center">
    <img alt="Constellation" src="https://raw.githubusercontent.com/alecmocatta/constellation/master/logo.svg?sanitize=true" width="550" />
</p>

<p align="center">
    A project to make Rust the cutting edge of distributed computing.
</p>

<p align="center">
    <a href="https://crates.io/crates/constellation-rs"><img src="https://img.shields.io/crates/v/constellation-rs.svg?maxAge=86400" alt="Crates.io" /></a>
    <a href="LICENSE.txt"><img src="https://img.shields.io/crates/l/constellation-rs.svg?maxAge=2592000" alt="Apache-2.0 licensed" /></a>
    <a href="https://dev.azure.com/alecmocatta/constellation/_build/latest?branchName=master"><img src="https://dev.azure.com/alecmocatta/constellation/_apis/build/status/tests?branchName=master" alt="Build Status" /></a>
</p>

<p align="center">
    <a href="https://docs.rs/constellation-rs/0.1.9">Docs</a>
</p>

Constellation is a framework for Rust (nightly) that aides in the writing, debugging and deployment of distributed programs. It draws heavily from [Erlang/OTP](https://en.wikipedia.org/wiki/Erlang_(programming_language)), [MPI](https://en.wikipedia.org/wiki/Message_Passing_Interface), and [CSP](https://en.wikipedia.org/wiki/Communicating_sequential_processes); and leverages the Rust ecosystem where it can including [serde](https://serde.rs/) + [bincode](https://github.com/servo/bincode) for network serialization, and [mio](https://github.com/tokio-rs/mio) and [futures-rs](https://github.com/rust-lang-nursery/futures-rs) for asynchronous channels over TCP.

Most users will leverage Constellation through higher-level libraries, such as:

 * **[Amadeus](https://github.com/alecmocatta/amadeus)**: Harmonious distributed data analysis in Rust. Inspired by [Rayon](https://github.com/rayon-rs/rayon), it provides a distributed process pool and built-in data science tools to leverage it.
 * With more in the pipeline!

For leveraging Constellation directly, read on.

## Constellation framework

* Constellation is a framework that's initialised with a call to [`init()`](https://docs.rs/constellation-rs/0.1.9/constellation/fn.init.html) at the beginning of your program.
* You can [`spawn(closure)`](https://docs.rs/constellation-rs/0.1.9/constellation/fn.spawn.html) new processes, which run `closure`.
* `spawn(closure)` returns the Pid of the new process.
* You can communicate between processes by creating channels with [`Sender::new(remote_pid)`](https://docs.rs/constellation-rs/0.1.9/constellation/struct.Sender.html#method.new) and [`Receiver::new(remote_pid)`](https://docs.rs/constellation-rs/0.1.9/constellation/struct.Receiver.html#method.new).
* Channels can be used asynchronously with [`sender.send(value).await`](https://docs.rs/constellation-rs/0.1.9/constellation/struct.Sender.html#method.send) and [`receiver.recv().await`](https://docs.rs/constellation-rs/0.1.9/constellation/struct.Receiver.html#method.recv).
* [futures-rs](https://github.com/rust-lang-nursery/futures-rs) provides useful functions and adapters including `select()` and `join()` for working with channels.
* You can also block on channels with the [`.block()`](https://docs.rs/constellation-rs/0.1.9/constellation/trait.FutureExt1.html#method.block) convenience method: `sender.send().block()` and `receiver.recv().block()`.
* For more information on asynchronous programming in Rust check out the [Async Book](https://rust-lang.github.io/async-book/index.html)!

Here's a simple example recursively spawning processes to distribute the task of finding Fibonacci numbers:

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
```

</details>
<p></p>

```rust
use constellation::*;
use serde_closure::FnOnce;

fn fibonacci(x: usize) -> usize {
    if x <= 1 {
        return x;
    }
    let left_pid = spawn(
        Resources::default(),
        FnOnce!([x] move |parent_pid| {
            println!("Left process with {}", x);
            Sender::<usize>::new(parent_pid)
                .send(fibonacci(x - 1))
                .block()
        }),
    )
    .block()
    .unwrap();

    let right_pid = spawn(
        Resources::default(),
        FnOnce!([x] move |parent_pid| {
            println!("Right process with {}", x);
            Sender::<usize>::new(parent_pid)
                .send(fibonacci(x - 2))
                .block()
        }),
    )
    .block()
    .unwrap();

    Receiver::<usize>::new(left_pid).recv().block().unwrap()
        + Receiver::<usize>::new(right_pid).recv().block().unwrap()
}

fn main() {
    init(Resources::default());

    println!("11th Fibonacci number is {}!", fibonacci(10));
}
```

<details>
<summary>Click to show output.</summary>

** TODO! This is the wrong screencap! **
![Screencap of constellation being used](http://deploy-rs.s3-website-us-east-1.amazonaws.com/deploy.gif)

</details>
<p></p>

Check out a more realistic version of this example, including async and error-handling, [here](examples/fibonacci.rs)!

## Running distributed

There are two components to Constellation:
 * a library of functions that enable you to `spawn()` processes, and `send()` and `recv()` between them
 * for when you want to run across multiple servers, a distributed execution fabric, plus the `deploy` command added to cargo to deploy programs to it.

Both output to the command line as show above – the only difference is the latter has been forwarded across the network.

Constellation is still nascent – development and testing is ongoing to bring support to Windows (currently it's Linux and macOS only) and reach a greater level of maturity.

The primary efforts right now are on testing, documentation, refining the API (specifically error messages and async primitives), and porting to Windows.

## Features
Constellation takes care of:
 * `spawn()` to distribute processes with defined memory and CPU resource requirements to servers with available resources
 * TODO: Best-effort enforcement of those memory and resource requirements to avoid buggy/greedy processes starving others
 * Channels between processes over TCP, with automatic setup and teardown
 * Asynchronous (de)serialisation of values sent/received over channels (leveraging [`serde`](https://crates.io/crates/serde), [bincode](https://github.com/servo/bincode) and optionally [`libfringe`](https://github.com/edef1c/libfringe) to avoid allocations)
 * Channels implement [`std::future::Future`](https://doc.rust-lang.org/std/future/trait.Future.html), [`futures::stream::Stream`](https://docs.rs/futures-preview/0.3.0-alpha.17/futures/stream/trait.Stream.html) and [`futures::sink::Sink`](https://docs.rs/futures-preview/0.3.0-alpha.17/futures/sink/trait.Sink.html), enabling the useful functions and adapters including `select()` and `join()` from [`futures-rs`](https://github.com/rust-lang-nursery/futures-rs) to be used, as well as compatibility with [`tokio`](https://github.com/tokio-rs/tokio) and [`runtime`](https://github.com/rustasync/runtime).
 * Powered by a background thread running an efficient edge-triggered epoll loop
 * Ensuring data is sent and acked before process exit to avoid connection resets and lost data (leveraging [`atexit`](http://pubs.opengroup.org/onlinepubs/000095399/functions/atexit.html) and [`TIOCOUTQ`](https://blog.netherlabs.nl/articles/2009/01/18/the-ultimate-so_linger-page-or-why-is-my-tcp-not-reliable))
 * Addressing: all channels are between cluster-wide `Pid`s, rather than `(ip,port)`s
 * Performant: designed to bring minimal overhead above the underlying OS

## What's it for
Constellation makes it easier to write a distributed program. Like MPI, it abstracts away sockets, letting you focus on the business logic rather than the addressing, connecting, multiplexing, asynchrony, eventing and teardown. Unlike MPI, it has a modern, concise interface, that handles (de)serialisation using [`serde`](https://crates.io/crates/serde), offers powerful async building blocks like `select()`, and integrates with the Rust async ecosystem.

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
This is a command [added to](https://doc.rust-lang.org/book/ch14-05-extending-cargo.html) cargo that under the hood invokes `cargo run`, except that rather than the resulting binary being run locally, it is sent off to the **bridge**. The **bridge** then sends back any output, which is output formatted at the terminal.

## How to use it
```toml
[dependencies]
constellation-rs = "0.1"
```
```rust
use constellation::*;

fn main() {
    init(Resources::default());
    println!("Hello, world!");
}
```
```text
$ cargo run
3fecd01:
    Hello, world!
    exited: 0
```
### Or, to run distributed:
Machine 2:
```bash
cargo install constellation-rs
constellation 10.0.0.2:9999 # local address to bind to
```
Machine 3:
```bash
cargo install constellation-rs
constellation 10.0.0.3:9999
```
Machine 1:
```bash
cargo install constellation-rs
constellation 10.0.0.1:9999 nodes.toml
```
nodes.toml:
```toml
[[nodes]]
fabric_addr = "10.0.0.1:9999" # local address to bind to
bridge_bind = "10.0.0.1:8888" # local address of the bridge to bind to
mem = "100 GiB"               # resource capacity of the node
cpu = 16                      # number of logical cores

[[nodes]]
fabric_addr = "10.0.0.2:9999"
mem = "100 GiB"
cpu = 16

[[nodes]]
fabric_addr = "10.0.0.3:9999"
mem = "100 GiB"
cpu = 16
```
Your laptop:
```text
cargo install constellation-rs
cargo deploy 10.0.0.1:8888 --release # address of the bridge
833d3de:
    Hello, world!
    exited
```

#### Requirements
Rust: nightly.

Linux: kernel >= 3.9; `/proc` filesystem.

macOS: Tested >= 10.10, may work on older versions too.

Please file an issue if you experience any other requirements.

## API

[see Rust doc](https://docs.rs/constellation-rs/0.1.9)

## Testing

[see TESTING.md](TESTING.md)

## Why?

Constellation forms the basis of a large-scale data processing project I'm working on. I decided to start polishing it and publish it as open source on the off chance it might be interesting or even useful to anyone else!

## License
Licensed under Apache License, Version 2.0, ([LICENSE.txt](LICENSE.txt) or http://www.apache.org/licenses/LICENSE-2.0).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be licensed as above, without any additional terms or conditions.
