# Troubleshooting

rava is meant to behave exactly like `java`: same output, same exceptions, same exit codes.
This page covers what to do when it does not: how to find the cause and how to report it.

## A program hangs: thread dumps

A thread dump lists every Java thread with its state and stack, in HotSpot's format. Two
threads blocked on each other's `synchronized` blocks are easy to spot in it, and so is a
thread parked forever.

| System | How |
|---|---|
| Linux, macOS | `kill -3 <pid>`, or `Ctrl+\` in the terminal running the program |
| Windows | `Ctrl+Break` in the console running the program |
| any, without a console | the environment variable `RAVA_DUMP_EVERY=<seconds>`: a dump every that many seconds |
| any, output not kept | also `RAVA_DUMP_FILE=<path>`: the dumps are appended to that file instead of standard output |

The dump goes to standard output. A launcher that keeps the program's output (Prism
Launcher's log, a server's console log) keeps the dump too. It looks like this:

```
Full thread dump rava:

"Render thread" outside Java (blocked or in native code)
	at jdk.internal.misc.Unsafe.park(Native Method)
	at java.util.concurrent.locks.LockSupport.park(LockSupport.java:371)
	at java.util.concurrent.CompletableFuture$Signaller.block(CompletableFuture.java:1898)
	...
```

Each thread is either *in Java* (running, or stopped at a safepoint for the dump) or *outside
Java* (blocked, waiting, or in native code). A thread that does not reach a safepoint within
two seconds is listed without its stack.

## A program fails: exceptions

An `UnsatisfiedLinkError` for a `java.*`, `sun.*` or `jdk.*` method means rava does not
implement that class-library native yet. The message names the method; please report it.
`RAVA_TRACE_ULE=1` prints each one to standard error as it happens, including those the program
catches.

For an exception that `java` does not throw, `RAVA_TRACE_THROW=<class name part>` prints where
the VM throws exceptions whose class name contains that text (for example
`RAVA_TRACE_THROW=ClassCastException`), with the innermost frames.

## Narrowing a bug down

These variables switch parts of the VM off, which tells you whether a wrong result comes
from a compiler:

| Variable | Effect |
|---|---|
| `RAVA_NO_TIER2=1` | no optimizing tier: the interpreter and the baseline compiler only |
| `RAVA_NO_JIT=1` | interpreter only (slow) |
| `RAVA_OPT=0` | hot methods stay in baseline code instead of the SSA optimizing tier |
| `RAVA_OPT_EXCLUDE=<prefixes>` | methods whose `package/Class.method` starts with one of the comma-separated prefixes are neither compiled by the SSA tier nor inlined into its code |

If a program works with `RAVA_NO_TIER2=1` or `RAVA_OPT=0` and fails without it, the optimizing
tier miscompiles something. Use `RAVA_OPT_EXCLUDE` to bisect: exclude `net/,org/` or
`java/,jdk/,sun/`, then halve whichever set fixed it, until you are down to one class or method
(`RAVA_OPT_EXCLUDE=net/minecraft/client/renderer/,com/mojang/blaze3d/`). Report that method.

`RAVA_GC_LOG=1` prints a line per collection. A program idle for ten seconds after a collection
gets one full collection that gives its heap back down to its live data (a server waiting for
players); `RAVA_IDLE_GC=0` turns that off. `RAVA_TIMING=1` prints how long the JDK took to
boot and the totals for the run. Both write to standard error, which some launchers do not keep
(Prism Launcher shows it only in its console window). `RAVA_STDERR=<path>` appends standard
error to that file instead.

## Crashes

When rava itself crashes (a memory fault nothing resolves, or a panic), it prints
`rava: crashed: ...` with the native and Java frames of the crashing thread. It writes the same
text to `rava_crash_<pid>.log` in its working directory (for a game, the instance's game
folder). It also prints a link to a new GitHub issue with those fields filled in, as bun does.
The link contains only module names and Java frames, no command line and no paths, and nothing
is sent until you submit the form. Attach the log file to the issue.

If a crash leaves only an exit code (`-1073741819` on Windows) and no `rava: crashed` report,
it happened inside a native library whose own exception handler ended the process first.
`RAVA_FAULT_EAGER=1` reports such a fault as it happens, and the report names the Java method
that called into the library. It is off by default because a library that raises and handles a
fault itself would then print a report for a fault it recovers from.

## Reporting

Open an [issue](https://github.com/redstone-md/rava/issues) with:

1. `rava -version`, and the system (Linux distribution, macOS or Windows version).
2. How the program was started: the command line, or the launcher and its settings.
3. The full output or log, not just the last lines; for a hang, a thread dump.
4. Whether `java` (HotSpot, same Java version) runs the same program correctly.

The most useful report is a small program that shows the difference. The
[semantic probes](../tests/semantics/) are programs of that kind, each one compared with
`java`.
