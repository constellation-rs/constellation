# Examples

A collection of simple examples leveraging the `constellation` framework.

They are invoked like:
```bash
cargo run --example example_name
```

Simply replace `example_name` with `fork_join`, `all_to_all`, or `process_pool`.

The number of processes is configurable at the command line like so:
```bash
cargo run --example example_name -- 42
```
to run for example 42 processes.

It can also be run distributed on a [`constellation`](https://github.com/alecmocatta/constellation)
cluster like so:
```bash
cargo deploy 10.0.0.1 --example example_name -- 1000
```
where `10.0.0.1` is the address of the master. See [here](https://github.com/alecmocatta/constellation)
for instructions on setting up the cluster.

## [fork_join.rs]

A simple example of fork-join parallelism.

This example generates random SHA1 hashes for 10 seconds and then prints the lexicographically "lowest" of them.

By default, 10 processes are spawned (the *fork* of fork-join parallelism). These processes then loop for 10 seconds, hashing random strings, before sending the lowest hash found to the initial process. The initial process collects these lowest hashes (the *join* of fork-join parallelism), and prints the lowest overall.

## [all_to_all.rs]

A simple example of all-to-all communication.

This example spawns several processes, which then send each other a message. The communication is all-to-all; if there are `n` processes spawned, there will be `n(n-1)` messages.

By default, 10 processes are spawned. They are then sent the pids of the rest of the 10 processes. Messages – a tuple of `(source process index, destination process index)` – are then sent in a carefully ordered manner to ensure that sends and receives are synchronised to avoid deadlocking. The processes then print "done" and exit.

This is a naïve implementation; a better one would leverage asynchrony. There will be such an example shortly.

## [process_pool.rs]

A simple example of a process pool.

This example spawns a process pool, and distributes work across it.

By default, 10 processes are spawned for the pool. By default, 30 jobs – which sleep for a couple of seconds before returning a `String` – are spawned on the pool. As such they are round-robin allocated to the 10 processes of the pool. The initial process collects and prints the `String`s returned by each job.

This is a simple implementation; a more featureful version is [`amadeus`](https://github.com/alecmocatta/amadeus).
