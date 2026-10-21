# Profiling rava

rava records what a run does into one binary file and analyzes it with `rava-prof`, a command
line tool built with the VM. The recording is meant to answer, for any workload, where the time
goes and why: which code runs (interpreted, baseline or optimized, inlined where), what the
threads wait for, what the compilers did, and how it compares with a HotSpot run of the same
workload.

## Recording

```
RAVA_RECORD=run.rprof java -jar server.jar
```

`RAVA_RECORD` takes a path and, after commas, options:

| option | default | meaning |
|---|---|---|
| `interval=<µs>` | `1000` | time between samples of each thread |
| `mode=cpu` / `mode=wall` | `cpu` | `cpu` samples a thread only when it used the CPU since its last sample; `wall` samples every thread, blocked ones included, with their state |

The recorder runs on its own thread. It suspends each Java thread briefly, reads its registers
and walks its frame headers (inlined frames come from the call sites' inlining records), and
writes events to the file as it goes, so a crash or a kill loses at most the last second.

### Events

| event | content |
|---|---|
| sample | time, thread, state, CPU cycles since the thread's last sample, stack |
| thread | id and name (again when a thread renames itself) |
| thread CPU | each thread's total CPU cycles, once a second: exact CPU per thread and window, independent of sampling |
| code | each compiled method's address range, method and tier, as it is installed |
| module | each loaded native module's path and address range |

Planned next, in this order: compilations (duration, size, inlining decisions with their
reasons), deoptimizations (method, bci, reason), garbage collections and safepoints (pause per
thread), monitor contention and parking (with stacks), allocation samples.

### Thread states

A sample says where the thread was:

- `java`: in compiled Java code;
- `vm`: in the VM's own code (the interpreter, the runtime, the collector);
- `syscall`: in a system call (most often blocked: a wait, a sleep, I/O), named by the system
  function and the caller that made it;
- `native`: in another module (a JNI library, the C runtime).

### Stacks

A stack is a list of frames, innermost first; in optimized code the frames at the pc include
the methods inlined there (with their bytecode indices), so self time lands on the method
whose code ran, as with HotSpot's JFR. A Java frame names the method, the bytecode
index when known, the tier of the code (`interp`, `baseline`, `opt`, `osr`) and whether it was
inlined into its caller. A native frame names the module and the offset in it; `rava-prof`
names the VM's own functions from its PDB (Windows) when one sits next to the executable, as a
`release-diag` build has.

## File format

The file starts with the 8 bytes `RAVAPROF` and a little-endian `u16` version. Records follow,
each a kind byte, a LEB128 length and the payload, so a reader skips kinds it does not know.
Strings, methods, modules and stacks are interned: each is written once with an id, and later
records refer to the id. Integers are unsigned LEB128. The reader and writer live in
`crates/rava/src/profile/format.rs`; the version changes whenever a record's layout does.

## Analysis

```
rava-prof <command> [options]
```

| command | output |
|---|---|
| `summary <file>` | duration, CPU by state, code installed by tier, CPU per thread name |
| `threads <file> [--window <s>]` | cores busy per thread name per window (10 s) |
| `top <file> [--inclusive]` | frames by self (or inclusive) CPU seconds |
| `tree <file> <frame> [--depth <n>] [--min <%>] [--callers]` | the callees under a frame (its outermost occurrence), inclusive; with `--callers`, who calls it |
| `diff <a> <b> [--inclusive]` | two recordings' frames side by side, sorted by the difference |
| `folded <file>` | folded stacks (`thread;outermost;...;innermost ms`) for flame graph tools |
| `import-jfr <in.json> <out> [--period <ms>]` | a HotSpot recording, `jfr print --json --events jdk.ExecutionSample` of it, as a recording |

Options for the reports: `--thread <prefix>` keeps the samples of threads whose name starts
with it, `--state java|vm|syscall|native` of one state, `--from <s>` / `--to <s>` of a time
window, `--within <frame>` (`top`, `diff`) of the stacks holding that frame; `--top <n>` limits the rows (40); `--tiers` labels Java frames with their tier;
`--keep-lambdas` keeps the addresses in hidden classes' names (merged by default: each run
names its lambdas anew); `--no-symbols` leaves native frames as `module+offset`.

**Seconds.** Times are CPU seconds. A thread's CPU between two of its once-a-second counts is
shared by its samples in that second, so two recordings compare in seconds whatever their
sampling rates. An imported HotSpot recording has no CPU counts: each of its samples weighs the
sampling period (`--period`: 10 ms for JFR's `profile` settings, 20 ms for `default`). A
`mode=wall` recording's samples weigh their interval: seconds of thread time, waiting included.

**Self time** goes to the innermost frame holding compiled code (inlined frames are attributed
to the method they were inlined into), which both VMs can tell, so `diff` compares the same
thing even where they inline differently; `--leaf` gives it to the innermost frame instead.
Inclusive time counts each frame once per sample.

**Symbols.** Native frames in rava's own code are named from its PDB when one sits next to the
executable that ran (a `release-diag` build has one); system libraries are named from their
exports.

**Comparing with HotSpot:**

```
RAVA_RECORD=rava.rprof rava -jar app.jar
JAVA_TOOL_OPTIONS="-XX:StartFlightRecording=filename=hs.jfr,settings=profile -XX:FlightRecorderOptions=stackdepth=2048" java -jar app.jar
jfr print --json --events jdk.ExecutionSample --stack-depth 512 hs.jfr > hs.json
rava-prof import-jfr hs.json hs.rprof
rava-prof diff hs.rprof rava.rprof --thread "Worker"
```

JFR keeps 64 frames of a stack by default and drops the outer ones: deep recursion then loses
its callers' inclusive time, and `import-jfr` warns about such samples. Record with a larger
`stackdepth`, as above.

## Limits and plans

- A sample in optimized code names the methods inlined at its pc: while recording, the
  optimizing compiler keeps where each value comes from (method, bytecode index, the inlining
  chain) and hands the recorder a map from code offsets to those chains. Baseline code is not
  inlined into, so its pc names its method.
- Recording is implemented for Windows; Linux needs a signal-based capture of the same data.
- Next events: compilations (duration, size, inlining decisions and their reasons),
  deoptimizations (method, bci, reason), collections and safepoints (each thread's pause),
  monitor contention and parking with stacks, allocation samples; then `annotate` (a compiled
  method's instructions with their samples, its IR and bytecode), which replaces the old
  `RAVA_SAMPLE` sampler.