# Per-tick Paper measurements

This optional external plugin records Paper 1.21.1 `ServerTickEndEvent.getTickDuration()` in
milliseconds. It does not change VM policy, tick logic or thread checks. Capture commands run
on the main thread. The listener skips the command tick and the requested warmup ticks, then
stores duration/tick number in preallocated arrays. Sorting, entity/player counts and CSV/JSON
output run on the main thread after the captured window. End-event listener overhead and work
after the event are outside Paper's reported duration; the result is this API's measurement,
not the complete tick-loop wall time.

Build once against the same prepared Paper base used for every VM. Use JDK 21 and Python 3.11+:

```sh
JAVA_HOME=/absolute/jdk21 bench/tick-measure/build.sh /absolute/paper-base /absolute/build-output
python3 bench/tick_measure.py --base /absolute/paper-base \
  --plugin /absolute/build-output/TickMeasure.jar --out /absolute/new-results \
  --vm upstream /absolute/upstream/rava --vm candidate /absolute/candidate/rava \
  --vm hotspot /absolute/jdk21/bin/java --rounds 3 --phases 3 \
  --warmup-ticks 1000 --ticks 3000 --mode paced
```

The base must use loopback port 25599, query/RCON disabled, seed 20260930 (or `--seed` with the
same value as the base), and a simple `level-name`. The runner copies the base for each fresh
process and removes overworld, nether and end saves. All other configuration stays identical;
the plugin is added to each copy. A trusted JSON array via `--server-args-json` supplies identical
JVM arguments before `-jar` when needed; after a prepared Paper JAR only `nogui` is allowed.
Argument files and port/world/config overrides are refused; no shell interprets arguments. Output
must be outside BASE. `--affinity 2,3` pins
only the child process to explicitly allowed CPU numbers. Choose CPUs using the actual host's
topology, and record/keep the same CPU set across VMs. Do not run another benchmark or profiler
alongside timed runs.

Inputs alternate forward/reverse process order each round. `manifest.json` records binary,
plugin, server/library JAR and configuration SHA256s plus commands/counts. Supply a reviewed
`--source-manifest` to identify an uncommitted candidate; binary hashes alone do not prove which
source built it. Each run preserves stdout, server logs, crash reports, raw tick CSV, per-phase
JSON and command timing. Cleanup stops or, after a timeout, kills only the runner's direct child.
Failure exits nonzero. Output directories must be new, preventing accidental overwrite.

`paced` requires ordinary 20 TPS scheduling. `sprint` starts and arms capture atomically on the
main thread through Paper's tick manager, attests sprint state for every captured tick, and stops
its sprint after capture;
this measures flat-out throughput under a different schedule. Compare each mode separately.
Quantiles use nearest-rank definitions; `over_50ms` counts recorded ticks beyond the20TPS
budget. Keep p50/p95/p99 per phase and fresh process, rather than relabeling averages as p50 or
pooling all runs into a reassuring single percentile. Few hundred ticks validate the harness;
they do not establish a stable p99. Use sufficiently long windows and independent process runs.

The default 350-mob scenario has **zero connected players**. `forceload` keeps chunks loaded
but does not establish normal player-nearby full AI activation. Results include player counts
at arm/after capture and entity counts after capture. Those boundary counts are not a simulation
checksum or a continuous activation audit. Do not call this a full-AI or real-player benchmark.
For that follow-up, use genuine version-matched protocol clients on port 25599, fixed identities
and positions near the pen, wait for player join and loaded chunks, keep clients on separate
CPUs, and preserve joins/disconnects/client scripts and per-tick player-presence evidence.
Retain a separate zero-player control. Do not disable activation checks to manufacture load.
