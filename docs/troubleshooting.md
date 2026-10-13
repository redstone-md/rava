# Troubleshooting

rava aims to behave exactly like `java`: same output, same exceptions, same exit codes. When it
does not, this page helps find out why and report it.

## A program hangs: thread dumps

A thread dump lists every Java thread with its state and stack, as HotSpot prints it. Two
threads stopped on each other's `synchronized` blocks, or a thread parked forever, show up at
once.

| System | How |
|---|---|
| Linux, macOS | `kill -3 <pid>`, or `Ctrl+\` in the terminal running the program |
| Windows | `Ctrl+Break` in the console running the program |
| any, without a console | the environment variable `RAVA_DUMP_EVERY=<seconds>`: a dump every that many seconds |

The dump goes to standard output, so a launcher that keeps the program's output (Prism
Launcher's log, a server's console log) keeps the dump too. It looks like this:

```
Full thread dump rava:

"Render thread" outside Java (blocked or in native code)
	at jdk.internal.misc.Unsafe.park(Native Method)
	at java.util.concurrent.locks.LockSupport.park(LockSupport.java:371)
	at java.util.concurrent.CompletableFuture$Signaller.block(CompletableFuture.java:1898)
	...
```

Each thread is *in Java* (running, or stopped at a safepoint for the dump) or *outside Java*
(blocked, waiting, or in native code). A thread that does not reach a safepoint within two
seconds is listed without its stack.

## A program fails: exceptions

- **`UnsatisfiedLinkError` for a `java.*`, `sun.*` or `jdk.*` method**: a native method of the
  class library that rava does not implement yet. The message names it; please report it.
  `RAVA_TRACE_ULE=1` prints every one to standard error as it happens, including those a
  program catches.
- **An exception that `java` does not throw**: `RAVA_TRACE_THROW=<class name part>` prints
  where the VM throws exceptions whose class contains that text (for example
  `RAVA_TRACE_THROW=ClassCastException`), with the innermost frames.

## Narrowing a bug down

These switch parts of the VM off, to tell whether a wrong result comes from a compiler:

| Variable | Effect |
|---|---|
| `RAVA_NO_TIER2=1` | no optimizing tier: the interpreter and the baseline compiler only |
| `RAVA_NO_JIT=1` | interpreter only (slow) |

If a program works with `RAVA_NO_TIER2=1` and fails without it, the optimizing tier
miscompiles something; that is worth a report with the smallest program that shows it.

Other diagnostics: `RAVA_GC_LOG=1` prints a line per collection; `RAVA_TIMING=1` prints how
long the JDK took to boot and the run's totals.

## Reporting

Open an [issue](https://github.com/redstone-md/rava/issues) with:

1. `rava -version`, and the system (Linux distribution, macOS or Windows version).
2. How the program was started: the command line, or the launcher and its settings.
3. The full output or log, not just the last lines; for a hang, a thread dump.
4. Whether `java` (HotSpot, same Java version) runs the same program correctly.

A small program that shows the difference is the most useful report of all: the
[semantic probes](../tests/semantics/) are exactly that, each one compared with `java`.
