# Prepared execution and task-lane control

The previous goal turn made progress by completing the pending 39-probe suite
and recording fresh ordinary-JIT and prepared boot results. The sub-five-second,
no-adapt, no-snapshot objective remains unmet.

All controls below use executable SHA256
`C682F5C30A0E601CE86BAF04333AA9A54C176E422D0490884ED8D81AEE4B41A7`.
Fresh-world, process-start-to-Done boots ran sequentially, without diagnostics
or snapshots, on port 25599 with affinity 0x0FFF. Quiet performance counters
were unavailable. These are individual observations, not small-effect estimates.

| Mode | Wall s | CPU s | Artifact suffix |
| --- | ---: | ---: | --- |
| Ordinary JIT, default lane | 24.429 | 26.53125 | fd1ef9bc693f49b4b72334c5615e2041 |
| Ordinary JIT, lane disabled | 28.666 | 30.15625 | b4a6ff4f65a44220824861214acdc467 |
| Prepared + JIT fallback, lane disabled | 91.102 | 89.84375 | f2dbb62ebc7d476b911be923e24832ed |
| Prepared, JIT disabled | 140.774 | 138.46875 | 6cd1c59846564b0d837d095259de9755 |

Artifacts are `%TEMP%/rava-product-boot-<suffix>`, containing binary, fixture
hashes, environment, output and result. Both prepared controls enable string
constants, resolved target cache and cell pool. Ordinary controls have no
prepared directory. `RAVA_NO_GILFREE_RINGS=1` is the lane-only control.

Disabling the lane does not explain the prepared regression: ordinary JIT
still takes 28.666 seconds. Prepared execution with JIT fallback remains over
three times slower in the lane-disabled comparison. The AOT path itself and
the extra runtime traffic it generates therefore need attention alongside
uncovered-method fallback. Do not enable the lane for JIT-disabled AOT: its
worker ABI is explicitly unsupported by aot_run_input and rings.rs.

## Diagnostic comparison

Root under `%TEMP%`:
`rava-string-boundaries-server-a11a9b7f90b2468d8a00b5b0f2b729be`.
`coverage-profile` is prepared/JIT-off; `jit-coverage-profile` is ordinary JIT
with the lane disabled. Both use BootstrapBench, the same classpath and
`RAVA_DIAG=price,exec,aot`, dispatch census, PC sampling and a 30-second exit
deadline. Both processes are terminal with exit zero. The prepared process
exited by its diagnostic deadline without a completed bootstrap/hash.
The ordinary process completed BootstrapBench with 29,671 states and hash
`b9e7debe631b42b6`, reporting 21,190 guest milliseconds. This instrumented time
is not a product-wall measurement or a new performance baseline.

Prepared recorded 4,256,097 interpreted entries within the incomplete window;
ordinary JIT recorded 922,912 across its completed run. At the second prepared
census, DoubleArrayList.getDouble accounted for 1,024,626 interpreted entries;
Reference2ObjectArrayMap put/findKey each accounted for 433,545. These names
are diagnostic examples only, never production eligibility rules.

The native-backed representation guard still refuses those namespace bodies.
Lifting it is not justified: native_backing.rs documents receiver-representation
counterexamples. Their frequency is also much lower in ordinary JIT, so simply
compiling each body does not reproduce the optimized caller's behavior.

Prepared cold linking consumed 127,288,900 ns across 1,486 attempts. The sampled
trimmed estimates were 10.152 s execute, 2.569 s resolution, 1.933 s entry,
1.094 s packing and 1.863 s interpreter self. They are not additive CPU costs;
execute includes unmetered callbacks. Production release PC symbol attribution
is additionally limited by identical-code folding, explicitly warned by the
sampler. Do not identify a Rust root cause from its symbol labels alone.

Next work must compare structural call lowering and runtime transitions in
the optimized JIT with prepared code. Preserve representation and exception
guards; do not replace them with historical/native-claim observations or
class-name allowlists. Coverage alone has failed to recover the 27-30 s baseline.

## Primitive-load control and symbol-correct profile

The subsequent ordinary-JIT control kept the lane disabled and added only
`RAVA_NATIVE_LOADS=0`: 29.209 s wall, 30.640625 s CPU, RSS 1041715200 bytes.
Artifact suffix: `5b8fc9f0cc624bb887dbeba9fb220ea7`. Against 28.666 s, this
single observation does not support primitive bytecode loads as the principal
prepared regression. The flag does not disable all collection intrinsics.
Do not begin a large primitive-load port on an assumed large startup benefit.

Built the existing release-diag profile with `cargo rustc --profile release-diag
-p rava-cli --features next-gen-gc --target-dir D:/code/rava/target --
-C link-arg=/OPT:NOICF`. Build completed in 1m21s. Frozen diagnostic SHA256:
`EB49EB40F5C6AF0EE0E13EB52BE31FDBD23C6EF59BCD5DFFF6C36B7E305B0567`.
The production release executable was not replaced. Fresh matching preparation
compiled 311,889 methods and refused 37,186 across 30,464 classes.

Diagnostic root under `%TEMP%`:
`rava-prepared-cost-profile-69a55b3f1abc41599f723316ad7905c9`.
Preparation and sequential `prepared`, `jit`, and `strings` diagnostic runs
are all terminal with exit zero. They are not product timing measurements.
The ordinary diagnostic completed BootstrapBench with the same 29,671 states
and `b9e7debe631b42b6` hash; the prepared diagnostics ended at their 30-second
deadline without a final bootstrap hash. Environment files and PDB are retained.

With exact host symbols, the two prepared single-thread windows contain 1.3%
and 2.1% samples in registered compiled code. The second window's largest host
rows include aot_run_input (5.3%), jit_call_body (4.9%), aot_direct resolution
closure (4.7%), memchr (4.5%), invokevirtual (2.6%), jit_direct_call (2.5%),
allocation (2.2%) and argument packing (1.6%). The census and meter overhead
are visible too; these shares are not an uninstrumented CPU budget. The
all-thread section is separately available rather than attributing parked
worker time to the one sampled thread.

The follow-up `strings` run disables the dispatch census and meters, using
only a timer selector for the deadline, and conservatively scans stack words
for memchr callers. The two windows match 148 samples each (3.7% and 3.6%).
Most direct memchr caller edges remain unprovable because the call is indirect.
Innermost candidate frames repeatedly include arg_layout, owner_bits,
native_backing and loader lookups. These are candidates, not proven stacks.
The reverse-search samples also expose the repeated return-descriptor scans
in aot_run_input and aot_direct_call. Runtime metadata parsing and repeated
execution envelopes remain the concrete target; native-backing refusal must
not be removed merely because these bodies already have prepared objects.

The methods.tsv confirms getDouble, Reference2ObjectArrayMap.put and findKey
are already compiled in the prepared archive. Their interpretation is a runtime
representation gate, not missing bytecode coverage. This rules out compiling
those same bodies again as an answer to their high interpreted counts.
