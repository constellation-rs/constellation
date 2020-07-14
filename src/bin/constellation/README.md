# fabric

This is part of the [`constellation`](https://github.com/alecmocatta/constellation) project.


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
