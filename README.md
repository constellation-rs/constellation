# deploy

[![crate](https://img.shields.io/crates/v/deploy.svg?style=flat-square)](https://crates.io/crates/deploy)

```rust
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate deploy;
use deploy::*;

fn main() {
    init(Resources::default());

    let mut total = 0;
    for index in 0..10 {
        let greeting = format!("hello worker {}!", index);
        let worker_arg = WorkerArg{index,greeting};
        let pid = spawn(worker, worker_arg, Resources::default()).expect("Out of resources!");
        let receiver = Receiver::<usize>::new(pid);
        total += receiver.recv().unwrap();
    }

    println!("total {}!", total);
}

#[derive(Serialize,Deserialize)]
struct WorkerArg {
    index: usize,
    greeting: String
}

fn worker(parent: Pid, worker_arg: WorkerArg) {
    println!("{}", worker_arg.greeting);

    let sender = Sender::<usize>::new(parent);
    sender.send(worker_arg.index*100).unwrap();
}
```
```python
$ cargo run
3fecd01:
    total 4500!
$ cargo deploy 10.0.0.1:8888  # deployed to a distributed fabric cluster
833d3de:
    total 4500!
```

Deploy is an under-development crate comprising a library to aide in writing and debugging of distributed programs, as well as tooling to run them across a cluster.

It is currently Linux-only. The primary efforts right now are on documentation, testing, filing down rough API edges, and porting to macOS and Windows.

# Features
Deploy takes care of:
* `spawn()` to distribute processes with defined memory and CPU resource requirements to servers with available resources
* Best-effort enforcement of those memory and resource requirements to avoid buggy/greedy processes starving others
* Channels between processes over TCP, with trivial setup and automatic teardown
* Asynchronous (de)serialisation of values sent/received over channels (leveraging [`serde`](https://crates.io/crates/serde) and [`libfringe`](https://github.com/edef1c/libfringe))
* `select()` to select over receivability/sendability of channels
* Integration with [`futures`](https://github.com/rust-lang-nursery/futures-rs), thereby compatible with [`tokio`](https://github.com/tokio-rs/tokio)
* Powered by a background thread running an efficient edge-triggered epoll loop
* Ensuring data is sent and acked before process exit to avoid connection resets and lost data (leveraging [`atexit`](http://pubs.opengroup.org/onlinepubs/000095399/functions/atexit.html) and [`TIOCOUTQ`](https://blog.netherlabs.nl/articles/2009/01/18/the-ultimate-so_linger-page-or-why-is-my-tcp-not-reliable))
* Addressing: all channels are between cluster-wide `Pid`s, rather than `(ip,port)`s

# What's it for
Deploy makes it easier to write a distributed program. Akin to MPI, it abstracts away Berkeley sockets, letting you focus on the business logic rather than the addressing, connecting, multiplexing, asynchrony, eventing and teardown. Unlike MPI, it has a sane, modern, concise interface, that handles (de)serialisation using [`serde`](https://crates.io/crates/serde), offers powerful async building blocks like `select()`, and integrates with frameworks like [`tokio`](https://tokio.rs/).

# How to use it
This readme is a work in progres...

# API

[see Rust doc](https://docs.rs/deploy)

# Testing

[see TESTING.md](./TESTING.md)
