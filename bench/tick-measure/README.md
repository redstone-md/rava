# Per-tick Paper measurements

An optional plugin that records Paper 1.21.1's `ServerTickEndEvent.getTickDuration()` for every
tick, in milliseconds. It changes no VM policy, tick logic or thread checks. Capture commands run
on the main thread. The listener skips the tick that ran the command and the requested warmup
ticks, then stores each duration and tick number in preallocated arrays. Sorting, entity and
player counts, and the CSV and JSON output happen on the main thread after the capture window.

The figure is what this API reports, not the whole tick loop's wall time: the listener's own cost
and any work after the event fall outside Paper's duration.

Build the plugin once against the same prepared Paper base used for every VM, with JDK 21 and
Python 3.11 or newer:

```sh
JAVA_HOME=/absolute/jdk21 bench/tick-measure/build.sh /absolute/paper-base /absolute/build-output
python3 bench/tick_measure.py --base /absolute/paper-base \
  --plugin /absolute/build-output/TickMeasure.jar --out /absolute/new-results \
  --vm upstream /absolute/upstream/rava --vm candidate /absolute/candidate/rava \
  --vm hotspot /absolute/jdk21/bin/java --rounds 3 --phases 3 \
  --warmup-ticks 1000 --ticks 3000 --mode paced
```

## The base

The base uses loopback port 25599, has query and RCON disabled, seed 20260930 (or `--seed` with
the base's value) and a simple `level-name`. For each fresh process the runner copies the base,
removes the overworld, nether and end saves and adds the plugin. Nothing else in the
configuration changes.

`--server-args-json` takes a trusted JSON array of JVM arguments, passed unchanged before `-jar`.
After the Paper jar only `nogui` is allowed. The runner refuses argument files and port, world or
config overrides, and no shell interprets the arguments. The output directory must be new and
outside the base, so a run never overwrites an earlier one.

`--affinity 2,3` pins only the server process, to CPU numbers you list. Pick them from the host's
actual topology and keep the same set for every VM. Run nothing else, benchmark or profiler,
during timed runs.

## What a run keeps

Each round alternates the process order, forward then reverse. `manifest.json` records the
SHA-256 of the binaries, the plugin, the server and library jars and the configuration, along
with the commands and counts. A binary hash does not say which source built it, so give an
uncommitted candidate a reviewed `--source-manifest`. Every run keeps its stdout, server logs,
crash reports, the raw tick CSV, per-phase JSON and command timings. Cleanup stops the runner's
direct child, killing it only after a timeout. A failure exits nonzero.

## Modes and quantiles

`paced` needs ordinary 20 TPS scheduling. `sprint` starts the sprint and arms the capture in one
step on the main thread through Paper's tick manager, checks that every captured tick ran in the
sprint, and stops the sprint after the capture. It measures flat-out throughput under a different
schedule, so compare each mode only with itself.

Quantiles are nearest-rank. `over_50ms` counts recorded ticks over the 20 TPS budget. Report
p50, p95 and p99 per phase and per process; an average is not a p50, and pooling every run into
one percentile hides the spread. A few hundred ticks show that the harness works but are too few
for a stable p99, so use long windows and several independent processes.

## Players

The default scenario has 350 mobs and no connected players. `forceload` keeps the chunks loaded
but does not switch on the full AI that a nearby player would. The results include the player
count when the capture is armed and after it, and the entity count after it. Those counts are not
a simulation checksum or a record of AI activation through the run, so this is not a full-AI or
real-player benchmark.

A real-player follow-up would use version-matched protocol clients on port 25599, with fixed
identities and positions near the pen. It would wait for the players to join and their chunks to
load, keep the clients on separate CPUs, and keep the join and disconnect logs, client scripts and
per-tick player counts. Keep a zero-player control beside it, and leave the activation checks on.
