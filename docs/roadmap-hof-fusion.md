# Generic higher-order call optimization

## Boundary

Minecraft is an acceptance workload only. Production eligibility may use bytecode,
descriptors, control flow, effects, escape facts, GC state and call-site stability. It
must not use package/class/method identity, workload identity, body hashes or expected
checksums. Names and checksums are diagnostics/test oracles only.

## Tracer bullets

1. **Done.** Remove per-call temporary allocation from the existing capturing-lambda direct
   dispatch. Repack live captures straight into the compiled callee's bounded cell
   buffer; retain all guards, rooting, forwarding and the general fallback.
2. **Done.** Measure the generic `BinarySearchLambda` acceptance workload with the feature off
   and on. Require exact output, lower allocator traffic and a repeatable wall-time win.
3. **Done.** Add a default-off structural fusion plan for an
   adjacent LambdaMetafactory producer and static/special consumer. Admit it only when
   the SAM value is proven non-escaping and every dynamic call resolves without
   descriptor adaptation.
4. **Done.** Preserve safepoints, GC roots, exception order and serial dispatch fallback. Reject
   unknown calls, observable lambda identity, monitors, volatile/shared writes and
   unsupported bootstrap shapes.

Each production Rust file remains below 500 new/changed lines per increment. The first
increment changes no server/client state model and introduces no animation path, so the
TanStack/Zustand and GSAP rules are not applicable.

## Increment 1 result

The clean same-commit A/B preserved every checksum while reducing median hot time by
13.3% at two million searches. Mimalloc cumulative committed traffic fell by 8.3 MiB
at 200,000 searches. Isolated server acceptance improved from `Done (173.733s)` to
`Done (130.317s)` with a noise-sized peak-RSS delta. Full release tests passed.

## Increment 2 result

`RAVA_JIT_HOF_FUSION=1` enables only the structural path. The production gate reads
the LambdaMetafactory static implementation handle from classfile `BootstrapMethods`
before execution, then proves exact descriptors/targets, CFG, bytecode effects,
class-initialization state, and bounded frame facts. It rejects synchronized methods,
exception tables, adaptation, escaping/reference SAM values, unknown calls, allocation,
and unsupported fault-capable operations.

Five rotated two-million-search rounds preserved `CASES=263073` and
`SUM=2282806567497590031`:

- Fusion on: median `HOTNS=133 ms`, process wall `167.1 ms`, peak RSS `10.5 MiB`.
- Fusion off: median `HOTNS=5897 ms`, process wall `6126.7 ms`, peak RSS `629.5 MiB`.
- JVM: `HOTNS=91.998 ms`, process wall `153.4 ms`, peak RSS `42.5 MiB`.

Fusion is `44.34x` faster by hot time than the unfused RAVA path and removes about
`619 MiB` of peak RSS. It remains about `8.9%` behind the JVM in process wall time
while using about `32 MiB` less peak RSS.

An isolated 200,000-call probe compiled the real
`CubicSpline$Multipoint.findIntervalStart` site into an HOF plan from its classfile
bootstrap. Fusion on/off produced the same `-199.99966` checksum; this verifies
coverage and parity only, not launch-to-Done reach or speed.

`bench/RavaSplineBench.java` exercises the real `CubicSpline$Multipoint.apply`
with 500k warm-up and 2M timed calls. Three alternating process runs per arm:
the exact native fallback (HOF off) had median `2.830s`; generic HOF fusion had
median `2.319s`, with the exact sum `2170549814183309` in every arm (~18% lower
hot time for HOF). This is a focused method microbenchmark, not a
world-generation or server-boot result.

In a one-round full-server boot, the HOF gate emitted no plans before `Done`;
the spline target received no fusion plan before `Done`. With the exact
spline route disabled and fusion enabled, wall was `29.2s` versus `29.4s` OFF;
one pair is not a performance verdict. The <5s goal remains unmet.

The JIT's per-entry spline pin is now a per-Code compile check: if the native
fallback is enabled, compilation proceeds only when that Code produces a
non-empty structural HOF plan. The exact same plan map is passed into codegen;
an empty plan keeps the interpreter/worker fallback and records the method as
non-compilable, avoiding repeated admission work.

Runtime traces of `bench/RavaSplineBench.java` confirm HOF-off retains the
interpreter/native fallback, while HOF-on builds the plan and compiles
`findIntervalStart`. HotSpot and both Rava paths produced the same sum. This
proves the compile transition and parity only; the microbenchmark does not
establish a world-generation or launch-to-Done improvement.


The existing `ChunkFill` setup fails in Rava with `ArrayIndexOutOfBoundsException`
(`index -1`, length 10) before `SETUP_MS`, both with the spline native enabled
and disabled; HotSpot completes the same fixture. It cannot currently provide a
world-generation comparison.

A default-off stable-array-metadata tracer was rejected and completely removed:
seven interleaved rounds regressed median hot time `138 -> 142 ms` (`+2.9%`) and
wall `169.0 -> 171.3 ms` (`+1.36%`) with no RSS change.

The retained `RAVA_JIT_POSITIVE_DIV=1` tracer uses a generic CFG range proof for
nonnegative signed power-of-two division. It rejects every alternate branch/switch
entry into the guard/load/divisor/div sequence and all legacy subroutine flow.
Seven alternating pairs preserved the exact checksum and improved median hot time
`136 -> 128 ms` (`5.88%`) and wall `171.2 -> 154.9 ms` (`9.52%`) with unchanged
`10.5 MiB` RSS. A post-soundness-fix smoke retained `136 -> 129 ms`.

### Remaining identity gates

The HOF compile path is structural, but the spline optimization is not yet
identity-free. `Vm::invoke`, the worker fast path, and `jit_run` still admit or
deny the native fallback using the literal owner and method name. The per-Code
compile check described above is therefore only a safer fallback boundary; it
does not remove the identity gate.

Do not replace those checks with the descriptor alone. The native lower-bound
loop is equivalent only when the classfile proves the whole semantic chain:
the wrapper's argument/return flow, its LambdaMetafactory implementation,
the predicate's float-array comparison (including NaN behavior), and the
resolved `IntPredicate` consumer's lower-bound behavior. The existing HOF plan
proves enough to generate a generic compiled path when admitted, but it is not
by itself a proof that the Rust native may replace arbitrary matching calls.

The worker route is also material: the recorded ring traffic attributes
488K calls to `findIntervalStart`. Removing that arm requires evidence that
worker dispatch reaches a structurally admitted compiled body instead, or a
generic worker-safe proof for the native operation. Until then, preserve the
bit-exact bytecode fallback and keep this identity-removal work open.

Acceptance for a replacement: classfile/CFG/effect facts only (no owner,
method, package, workload, or execution-history allowlist); fail closed on
unknown consumers or shapes; identical spline benchmark checksum; and a
worker-path test showing the old named arm is not needed.

## Multicore boundary

Boot and world generation are CPU-serial by construction: `run_main_with_args` holds
one global GIL, daemon Java threads are cooperative by default, an opt-in OS daemon
still acquires the same GIL, and Executor/scheduler/CompletableFuture async paths call
`vm.apply` inline. `RavaPool` is genuinely GIL-free only for exact structural primitive
reductions/maps/scans with independent effects and serial fallback.

This is a runtime ownership boundary, not an OS scheduling accident. Dispatching
arbitrary `vm.apply` calls to workers would race execution state, resolution, class
initialization, the moving heap and GC.

## No-adapt-safe multicore roadmap

Eligibility may use only standard concurrency contracts, descriptors, CFG, effects,
escape/ownership facts, initialization state and linked-target stability.

0. Measure flat bootstrap separately from a fresh compute-heavy world benchmark; record
   CPU/wall ratio, task counts, GIL contention, forks, aborts and serial retries.
1. Restore exact Executor/ForkJoin/CompletableFuture async semantics on OS workers while
   each guest task still acquires the GIL.
2. Move exceptions, calls, trampoline state, current thread, frames/JIT roots and
   transient buffers into a per-thread runtime.
3. Expand `RavaPool` only through canonical counted CFG, primitive capture/result,
   disjoint-index or associative-fold proofs with abort-before-publication fallback.
4. Add detached tasks with private nurseries and copy-on-write overlays against a frozen
   shared epoch; conflict, overflow or unsupported effects rerun serially.
5. Connect structurally eligible executor/future tasks to that detached runtime and
   leave every other task on the GIL path.

Do not auto-parallelize class initialization or remove the GIL wholesale. No phase may
admit work through workload names, package/class/method identity, body hashes or
expected outputs.
