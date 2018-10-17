# fabric

This is part of the [`deploy`](https://github.com/alecmocatta/deploy) project.


## `fabric`
Run a fabric worker, optionally as master, and optionally start a bridge running.

## Usage
```text
fabric master (<addr> <mem> <cpu> [<bridge> <addr>]...)...
fabric <addr>
```
## Options
```text
-h --help          Show this screen.
-V --version       Show version.
```
A fabric cluster comprises one or more workers, where one is declared master.

The arguments to the master worker are the address and resources of each worker,
including itself. The arguments for a worker can include a binary to spawn
immediately and an address to reserve for it. This is intended to be used to
spawn the bridge, which works with the deploy command and library to handle
transparent capture and forwarding of output and debug information.

The first set of arguments to the master worker is for itself – as such the
address is bound to not connected to.

The argument to non-master workers is the address to bind to.

For example, respective invocations on each of a cluster of 3 servers with
512GiB memory and 36 logical cores apiece might be:
```text
fabric 10.0.0.2:9999
```
```text
fabric 10.0.0.3:9999
```
```text
fabric master 10.0.0.1:9999 400GiB 34 bridge 10.0.0.1:8888 \
              10.0.0.2:9999 400GiB 34 \
              10.0.0.3:9999 400GiB 34
```
Deploying to this cluster might then be:
```text
deploy 10.0.0.1:8888 ./binary
```
or, for a Rust crate:
```text
cargo deploy 10.0.0.1:8888
```

<!-- [package]
name = "fabric"
version = "0.1.2"
license = "Apache-2.0"
authors = ["Alec Mocatta <alec@mocatta.net>"]
categories = ["development-tools","network-programming","concurrency","command-line-utilities"]
keywords = ["deploy","distributed","fabric"]
description = """
A distributed fabric for `deploy`able programs.
"""
repository = "https://github.com/alecmocatta/deploy"
homepage = "https://github.com/alecmocatta/deploy"
documentation = "https://docs.rs/fabric"
readme = "README.md"

[dependencies]
deploy-common = {path = "../common", version = "=0.1.2"}
bincode = "1.0"
serde = "1.0"
serde_derive = "1.0"
serde_json = "1.0"
crossbeam = "0.4"
either = "1.5"
palaver = {git = "https://github.com/alecmocatta/palaver"}

[target.'cfg(unix)'.dependencies]
nix = "0.11"

[target.'cfg(windows)'.dependencies]
winapi = "0.3"
 -->