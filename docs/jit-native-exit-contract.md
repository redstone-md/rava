# JIT native-exit failure contract

Compiled callbacks cannot unwind a Rust `VmError` through generated machine
code. The per-thread execution state therefore keeps two mutually exclusive
first-failure channels:

- guest `Throw(Value)` stays in the registered GC root;
- every other `VmError` stays in a private TLS failure slot.

The existing pending bit mirrors either channel. Callback guards stop after any
failure, while exception handlers inspect only the rooted `Throw` channel. The
outer JIT and linked-AOT boundaries consume the combined error and return the
original `VmError`. The common successful path remains a bare pending-bit test.

`rava_deopt` now accepts only `RunExit::Normal`. A nonterminal interpreter exit
is converted to an explicit `UnsupportedNative` failure, and an interpreter
error is preserved instead of becoming the native cell value `0`. The callback
also latches the JIT fault so later generated side effects do not run. A
GIL-free worker aborts before reading the site or VM pointer, and deopt cache
invalidation rejects every worker independently of experimental kill switches.

Handler-bearing compilation remains closed. The next bounded JIT slice must use
a separate exception-exit callback because deopt supersedes the compiled body
and repeats the faulting bytecode. Initial admission is limited to a serial,
memory-ABI static method with one catch-all range, one terminal `athrow` site and
a straight-line interpreter handler.

## Validation

The execution-state tests and two real `rava_deopt` callback tests pass with
`next-gen-gc`, including an early worker abort with deliberately null native
pointers. `AotExceptionSem` matches all 13 Java `-Xint` output lines on the final
release. The pre-audit release reached `Done` in an isolated fresh-world
first-entry boot on port 25599. Its 36.022-second wall observation had
unavailable quiet counters and is only a correctness smoke; the final correction
changes only the worker refusal boundary.

Candidate SHA-256:
`4217B7E311F11036F88F75CEE155D6DA61F7150356196AC71435A9CFD4055F85`.
Preserved control SHA-256:
`05ADCBE8983DE95EED4E0275E336AAE44B92C4E0C4070D4807730AA11D1CDB6C`.
Evidence: `%TEMP%/rava-native-exit-current.txt` points to the frozen binaries,
patch, build and validation logs.
