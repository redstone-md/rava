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
| any, output not kept | also `RAVA_DUMP_FILE=<path>`: the dumps are appended to that file instead of standard output |

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
| `RAVA_OPT=0` | hot methods stay in baseline code instead of the SSA optimizing tier |
| `RAVA_OPT_EXCLUDE=<prefixes>` | methods whose `package/Class.method` starts with one of the comma-separated prefixes are neither compiled by the SSA tier nor inlined into its code |

If a program works with `RAVA_NO_TIER2=1` or `RAVA_OPT=0` and fails without it, the optimizing
tier miscompiles something. `RAVA_OPT_EXCLUDE` finds what by halving: exclude `net/,org/` or
`java/,jdk/,sun/`, then the half that fixed it, down to one class or method
(`RAVA_OPT_EXCLUDE=net/minecraft/client/renderer/,com/mojang/blaze3d/`). That method is worth a
report.

Other diagnostics: `RAVA_GC_LOG=1` prints a line per collection; `RAVA_TIMING=1` prints how
long the JDK took to boot and the run's totals.

## Crashes

When rava itself crashes (a memory fault nothing resolves, or a panic), it prints
`rava: crashed: ...` with the native frames and the Java frames of the crashing thread, writes
the same to `rava_crash_<pid>.log` in its working directory (for a game, the instance's game
folder), and prints a link to a GitHub issue with those fields filled in, as bun does. The link
holds no command line and no paths, only module names and Java frames, and nothing is sent until
you submit the form; attach the log file to it.

A crash that ends with only an exit code (`-1073741819` on Windows) and no `rava: crashed`
report happened inside a native library whose own exception handler ended the process first.
`RAVA_FAULT_EAGER=1` reports such a fault as it happens, which names the Java method that called
into the library. Off by default: a library that raises and handles a fault itself would print a
report it recovers from.

## Reporting

Open an [issue](https://github.com/redstone-md/rava/issues) with:

1. `rava -version`, and the system (Linux distribution, macOS or Windows version).
2. How the program was started: the command line, or the launcher and its settings.
3. The full output or log, not just the last lines; for a hang, a thread dump.
4. Whether `java` (HotSpot, same Java version) runs the same program correctly.

A small program that shows the difference is the most useful report of all: the
[semantic probes](../tests/semantics/) are exactly that, each one compared with `java`.
