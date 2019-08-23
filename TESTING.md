# Testing

constellation has its own testsuite, in [tests/tester/main.rs]. It can be invoked with:
```
cargo test
```

### Valgrind

The testsuite can be run under valgrind's memcheck tool like so:
```
valgrind --tool=memcheck --trace-children=yes --gen-suppressions=yes --quiet --child-silent-after-fork=yes --trace-children-skip=\*cargo target/debug/test
```
`--trace-children=yes` ensures that child processes are also run under valgrind<br/>
`--gen-suppressions=yes` ensures that valgrind pauses execution on detecting an error<br/>
`--quiet` and `--child-silent-after-fork=yes` disable printing of valgrind/memcheck informational output which the tests do not expect<br/>
`--trace-children-skip=\*cargo` disables valgrind for the invocation of `cargo build` under the hood.
