# Rava as an SDK: the tooling roadmap

Rava is meant to be a JRE **and** a JDK — a developer who targets it should get a debugger, a
profiler and a diagnostics surface at least as good as the ones a JVM ships. This document is
the plan for that, and it exists because the engine's own optimisation work kept failing for
tooling reasons, not for want of ideas.

## Why this is a performance document, not only a product one

Every measurement in the last session cost a 12-minute product boot, and seven separate times
the answer was *"the instrument printed from a path this run never reaches"*:

| table | printed only from | why a product boot never saw it |
|---|---|---|
| `icmiss`, `backedge`, `dresolve`, `vecgrow`, `picprobe` | `lane_probe` | the harness kills the process at `Done` |
| `RAVA_JIT_CB_CENSUS` (the seam ranking) | `pc_profile` dump | only on a profiler build |
| `JIT_GATE` / `COLD_LEVEL` | `lane_probe` | same as above |
| every end-of-run census | `run.rs` tail | a Minecraft server exits inside `System.exit`, not by returning from `main` |

Four separate dead ends in the map-key arc died on **one field id**, and only a layout dump
settled it. A counter split by *reason* cost one build and would have saved four.

⭐**The lesson is structural: a diagnostic that depends on how the program ends is not a
diagnostic.** Everything below follows from that.

## Principles

1. **One registry.** Every counter, histogram and timer registers by name in one place, with a
   category. No more per-site `static AtomicU64` + private env var + bespoke `eprintln!`.
2. **Exit-independent output.** A dump must happen on: a timer, `System.exit`/`Runtime.halt`,
   normal return from `main`, a panic, and an external signal. If any one of those is the only
   path, the instrument is broken.
3. **Zero cost when off.** One relaxed load on the armed path, nothing else. The existing
   censuses already meet this bar and the new registry must not regress it.
4. **Machine-readable.** Text for a human, JSON for a tool. A boot A/B should be able to diff two
   dumps mechanically instead of by eye.
5. **Named populations, not percentages.** The engine's profile is flat — top host function
   3.4% — so the useful instruments are the ones that name *who* and *how many*, not *how long*.

## Stages

### Stage 1 — `diag` core (this change)
- `crate::diag`: registry, `Counter` / `Histogram` / `Timer` handles, category tags.
- Selection: `RAVA_DIAG=<comma list or `all`>`, matched against `category.name`.
- Output: `RAVA_DIAG_OUT=stderr|<path>`, `RAVA_DIAG_FORMAT=text|json`,
  `RAVA_DIAG_EVERY_MS=<n>` for periodic dumps.
- Dump wired to all five exit paths above.
- Tracer bullet: migrate the seam census, the JIT gate counters and the key-derivation split, so
  the shape is proven end to end before the other twenty follow.

### Stage 2 — migrate the existing instruments
Roughly thirty censuses exist today, each with its own flag and print site. They move to the
registry, keeping their current env var as an alias so no recorded measurement stops working.

### Stage 3 — the profiler as a product
`pc_profile` already samples host and compiled frames and resolves symbols. What it lacks is a
stable output format and a way to ask for a *phase*. Give it:
- `--profile=<out.json>` on the CLI, not only an env var;
- phase markers the guest can emit (and that MC's own log lines already imply);
- flamegraph-compatible output.

### Stage 4 — the debugger
JDWP over a socket is the interoperable answer: every IDE speaks it. The engine already has the
pieces a debugger needs — a frame stack it can walk, a class loader that can name a method, and
a safepoint the interpreter reaches ~500 times a second. What it lacks is the protocol layer and
breakpoint support in `run_hot`.

### Stage 5 — the JDK surface
`javac` is out of scope, but the *tools* are not: `rava-jps`-style process listing, a heap
summary, a thread dump on signal. These are what make an engine usable by someone who did not
write it.

### Stage 6 — a reproducer harness

The threshold-1 hunt ended where every deep engine bug ends: **a 40-second Minecraft boot is the
wrong reproducer.** Each hypothesis cost ten minutes, and the decisive observation was that
`RAVA_TRACE_COMPILE=1` makes the fault vanish — a timing race. What the engine needs is a way to
express "fill this inline cache with a deep entry and a leaf entry while a background compile is
in flight" as a test that runs in a second.

That is the same shape as the existing bit-exact gates, one level lower: gates prove a whole
program's output; this proves one interaction. Neither exists today for the JIT.

## Status

Stage 1 shipped: registry, gauges, selector, five dump triggers, delta time series, and the
`exec.compiled` / `exec.interpreted` coverage pair. It has already closed one line (coverage is
not the threshold), corrected one of my own readings (`RAVA_DIAG=all` perturbs the boot it
measures), and narrowed a miscompile from "somewhere at threshold 1" to "the Deep-IC arm, and it
is a race" in six runs.

**Stage 1b shipped: METERS.** A counter answers *how many*; nothing in the registry answered
*how much does it cost*, so every conversion from a population to a share of the boot — the
tree's own first rule — was an estimate multiplied by a real number, which reads exactly like a
measurement. `diag_meter!` wraps a site with an RAII guard, samples 1 in 256, subtracts its own
calibrated `rdtsc` overhead and prints events, p50, mean, estimated seconds and % of wall.

It has already corrected three of its own readings and one of the tree's:

* mean → median, after disagreeing 10x with a six-boot CPU A/B (rdtsc counts WALL cycles, so a
  preempted sample poisons a mean);
* counters ignored `RAVA_DIAG_DELTA` while gauges honoured it, so a series printed totals and
  rates under one banner;
* meters ignored delta mode altogether, which is what makes a series a phase map;
* and the finding that came out of it: **the boot and the post-`Done` tick differ by 4x in
  crossings**, so every census taken over a capped window mixes two workloads — which is what
  made a 9.06 M population look like a boot cost when 86% of it was ticking.

Stages 2-6 not started.
