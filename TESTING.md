# Testing

deploy has its own testsuite, in test/src/main.rs. It can be invoked with:
```
cargo run -p test --bin test --release -- --release
```
Note that under the hood, test invokes cargo build with the same arguments given to it. Hence the `--release` after the `--`, which `cargo run` forwards to test, is in turn forwarded to the invocation of cargo build.

### Valgrind

The testsuite can be run under valgrind's memcheck tool like so:
```
valgrind --tool=memcheck --trace-children=yes --gen-suppressions=yes --quiet --child-silent-after-fork=yes --trace-children-skip=\*cargo target/release/test --release
```
`--trace-children=yes` ensures that child processes are also run under valgrind<br/>
`--gen-suppressions=yes` ensures that valgrind pauses execution on detecting an error<br/>
`--quiet` and `--child-silent-after-fork=yes` disable printing of valgrind/memcheck informational output which the tests do not expect<br/>
`--trace-children-skip=\*cargo` disables valgrind for the invocation of `cargo build` under the hood.
