# What was measured and CLOSED on 2026-08-25

Nine commits. No boot win. The value is the eliminations: every external explanation for
rava's 7x boot gap was tested and closed with a number, so none of them should be
re-derived. Standing numbers at the end of the session:

    rava   wall 48.25 s   CPU 52.3 s   1.07 cores   Done 0.44 s   RSS 1.35 GB
    java   wall  6.85 s   CPU 21.7 s   3.16 cores   Done 0.20 s   RSS ~0.35 GB

Phase split (product, JIT on): start -> `Environment:` 30 s (61%) | reload 18 s (37%,
18x off java) | reload -> `Done` 1 s. Whole-boot wall buckets: JIT_EXEC 31.0% |
NATIVE_BRIDGES 26.0% | JIT_CALLBACK 25.1% | INTERP_EXEC 11.5% | VM_DISPATCH 5.6% |
CLASS_LOADING 0.7%. RIP sampling of the same boot: **8.2% inside compiled code, 91.8% in
the host** — the two profilers answer different questions and must not be mixed.

## Closed by measurement

**The GIL is not the serializer.** `RAVA_GILSTAT=1 RAVA_CENSUS=1` on a real boot:
`acquires=55 contended=1 blocked_total=0.0ms real_threads=1`. `gil.rs:34` had already
written down what such a reading would mean. The real cause of 1.07 cores is
`reflect/concurrent.rs:383` — every daemon becomes a cooperative green thread by design
("correct, single-core") — plus `task_runtime.rs`'s one-guest-task-per-GIL-acquisition.

**The deferral lane is not the lever.** It admits the datapack reload and every task
aborted; two of the six named abort sites were cold start rather than races and are fixed
(`b51e21d9`). But the reload's fan-out mostly never reaches the lane, and the lane's own
region is a sliver of the phase.

**Dispatch misses are not the lever, and this is the strongest result.** The heap-Ref PIC
(default-on) fills 18 entries and refuses 19,856,228, because `dispatch.rs:298` rules
`cls.starts_with("java/") => NeverCacheable` — correct, since this tree has no `java/util`
bytecode. Serving the top two populations (`Iterator#hasNext/next` via the existing
`RAVA_ITER_INTRINSIC`, `Map$Entry#getKey/getValue` via the new `FuKind::Entry`) took
refusals **19,856,228 -> 11,596,004**, verified by census. Product-config A/B, n=4 each:
**0.00%** and **-1.12%**, not disjoint. Removing SEVEN MILLION host excursions moves a
48 s boot by less than the floor, so a miss costs under ~10 ns — not the ~190 ns obtained
by dividing the `JIT_CALLBACK` bucket by the miss count.

**Replacing java.util with natives is not the mistake.** `jimage extract` put 8.6 MB of
real JDK 25 `java.util` bytecode on the classpath (resolution already prefers bytecode).
`List.size` went **65 -> 369 ns** — the JDK's own bytecode runs 5.7x SLOWER here than our
native — and then died on `UnsatisfiedLinkError: jdk/internal/util/Preconditions.checkIndex`,
because `java.base` needs a `jdk.internal.*` surface we do not have.

**Codegen is not the cause either.** `FrameBench`, 40 M calls, today's binary: rava JIT
**1.1 ns/call**, java JIT 0.8, `java -Xint` 39.8. Our compiled static call is 1.4x off
HotSpot and 36x faster than the JVM's interpreter. The ledger's "frame 331 ns vs 17.4 =
19x" measured a different, non-inlinable call shape and must not be generalised.

**`RAVA_DISPATCH_GATE`** — neutral, now twice (523/540/542 ns off vs 543/545/561 on, plus
the older boot A/B). The six family dispatchers early-out cheaply on the owner; computing
`plan(owner)` to skip them costs what it saves.

## Instruments repaired (all had never printed on this workload)

`heappic::dump()` had NO caller at all. `bridgestat`'s dump period was hard-coded to 60 s
against a 48 s boot. `icmiss`'s clause census could not see the 19.4 M refusals because
`pic_fill` short-circuits them earlier — clause 6 `jdk-bail` records them at the bail.
That is instances six, seven and eight of the defect ledger row 393 first recorded.

New price lists: `bench/BridgeCost.java` (per-native cost over the population the boot's
`[WALL-BRIDGE]` tables name), `bench/EqProf.java` + `bench/StaticCost.java` (what ONE
bridge call pays: routing ~35%, plumbing ~39%, body ~26%; `Math.max` 7 ns vs
`Objects.equals` 805 ns proves inline emission works), `bench/MapKeyProf.java`,
`bench/HashProf.java`, `bench/R2OCost.java`.

## Rules this session paid for

1. **Count x assumed unit cost is not pricing.** Only a same-binary flag A/B, in a
   configuration where the mechanism can actually run, prices a lever.
2. **`bench/boot_ab_flag.ps1` takes `-Jit` as a SWITCH, default OFF.** Two A/Bs were run
   against structurally inert compiled-code paths before this was noticed; one produced a
   spectacular -48.24% that was pure box variance.
3. **Inclusive is not self.** `[BRIDGE-CALLER]` attributes to the OUTERMOST bridge:
   `List#forEach` shows 2.88 s self on EIGHT calls because the guest body runs under it.
4. **Read a census only with its probe armed.** `[DISPATCH-TABLE]`'s `mc_hits/serves/fills`
   are picprobe-gated and read 0 otherwise; `FILLS` counts only actual writes, so a low
   number means "few distinct facts", not "learned nothing".

## What is left

Everything external is closed. What remains is not one lever but the flat profile itself:
top host row 4.4%, no single mechanism above ~2% of the boot, and ~50 of them. The two
shipped perf changes of this session (`entrySet` view caching, `heap::Syn` constant field
ids) each cut their microbenchmark by 35-66% and moved the boot by 0.00% — which is the
same statement one more time.

## Correction to a standing record, and the one task it leaves

The ledger carried `RAVA_ARENA_MAPS` as "+48% on the boot" — a regression to be explained.
Re-measured in the configuration that can see it (`-Jit`): n=3 gives **-8.82%** with SET
winning all three pairs, n=5 gives **-2.00%** with three of five; six of eight pairs
overall, min 52.7 vs 54.9 = -4.0%, not disjoint.

None of it counts. The flag **FAILS 4 GATES OF 4** —
`IllegalStateException: Errors during registry creation` on ChunkFill and RingScan — while
the server boot still reaches `Done`, which is exactly why its timings look plausible and
why the old "+48%" was equally meaningless. Any timing of this flag is void until it
passes the gates.

That matters because the flag is the BUILT form of the one cure these eliminations point
at. RIP sampling puts **8.2% of the boot inside compiled code and 91.8% in the host**;
HotSpot's ratio is the inverse. The cure is to do the runtime's work inside machine code,
and the machinery already exists — `emit_amap_int_probe`, `bitset_get_inline`, the
`Int2ObjectOpenHashMap` register probe — behind a flag that is broken rather than slow. So
the next task is "fix `RAVA_ARENA_MAPS` until the gates pass", not "find another lever".

Object representation is the other half of the same story. `RAVA_CENSUS`, read in full for
the first time: RSS ~1.0 GB against java's ~0.35 GB, of which Rust-side bookkeeping is
345 MB — `obj.desc_table` 59.6 | `array obj table` 48.1 | `field blob` 34.7 |
`obj.DEAD structs` 30.4 | `obj.table` 27.8 | `obj.struct` 21.2 — and **90.7% of live
arrays hold <= 8 elements, paying a 48.1 MB spine for 16.8 MB of payload**, with 82% of the
handle table free-but-retained. Not acted on, by the conversion rule: allocation is
`mi_malloc` 4.2% + `mi_free` 0.7% + `Heap::gc` 2.2% of the boot, so halving it lands under
the floor.

## The arena-representation family, mapped

`RAVA_ARENA_MAPS` was repaired this session (four correctness defects, above) and now passes
4 gates and 19 probes. That prompted the obvious question: the other five flags each move a
class family off the handle heap, which is taxed twice (345 MB of Rust-side bookkeeping, and
a `java/*` or heap-Ref receiver that can never be PIC-cached), so does the SET cross a
threshold none of its members does? Measured:

    every flag ALONE                     boots, CPU 51.3 - 54.3
    MAPS + RECORD + ENUM                 boots, CPU 52.0
    SYNTH + NATIVE_NEW + OVERRIDE        HANGS  (150 s timeout, no `Done`)
    ALL SIX                              HANGS
    NATIVE_NEW + OVERRIDE                boots, CPU 50.7

`NATIVE_NEW + OVERRIDE` then measured **-3.54% CPU with the ON arm winning 4 of 4 pairs**
(50.1/50.2/51.3/49.1 against 53.0/50.9/51.8/51.4), every arm reaching `Done` — the best
paired reading of the session. **And it fails 4 gates of 4**: ChunkFill hangs outright.

So the reading is void, exactly as `RAVA_ARENA_MAPS`'s own "+48%" and "-8.82%" were before
its repair. The pattern is the finding: **this family is opt-in because it is BROKEN, and
its timings look attractive precisely because a broken arm does less work.** A boot reaching
`Done` is not evidence — the boot completed for the pair that hangs ChunkFill, and for
`RAVA_ARENA_MAPS` while it was silently discarding every view-mediated removal.

Practical consequence for anyone continuing: never quote a timing from this family without
the four gates beside it, and repair in the order `MAPS` was repaired — write a probe for the
contract you suspect, confirm the probe FAILS, fix, confirm it passes, then gates, then
timing. Three of the four `MAPS` defects were invisible to the gates and to 431 unit tests;
only the probes found them.

### …and then the family closed too

Bisected on ONE gate, one flag at a time, which is what the pair's `-3.54%` deserved:

    (none)                ChunkFill OK  FILL_MS=8358
    RAVA_ARENA_NATIVE_NEW ChunkFill OK  FILL_MS=7864   <- passes, and 5.9% faster ON THE GATE
    RAVA_ARENA_OVERRIDE   HANGS (200 s, no hash)
    NATIVE_NEW+OVERRIDE   HANGS

So `OVERRIDE` is the broken one — its own comment already said it "stays opt-in until a full
boot bit-exact validation" — and the pair's attractive `-3.54%` with 4-of-4 pairwise wins was
the broken flag doing less work, precisely as predicted one paragraph earlier.

`NATIVE_NEW` alone is CLEAN: 4 gates bit-exact, 19 of 19 probes. Measured properly it is
**-1.61% CPU, 2 of 5 pairwise, not disjoint** — below the floor. And the two flags that are
now correct, composed:

    RAVA_ARENA_MAPS (repaired)                -2.53%  3/5  at the floor
    RAVA_ARENA_NATIVE_NEW                     -1.61%  2/5  below it
    MAPS + NATIVE_NEW                         -0.20%  2/4  NEUTRAL

**Moving object families off the handle heap is measurably neutral on this boot** — despite
345 MB of Rust-side handle bookkeeping and despite those receivers being permanently
PIC-uncacheable. That closes the representation hypothesis at the level that is testable
today: the two remaining members are broken, and the two that work compose to nothing.

## The verdict this session's numbers support

The goal is a boot in single-digit seconds. Here is what the measurements say it would take,
using only figures established above.

**Work per core.** rava spends 52.3 CPU-s on the boot. java spends 21.7 CPU-s, but most of
that is its JIT compiler threads: its wall is 6.85 s at 3.16 cores, and its application
thread accounts for roughly 6.5 CPU-s. So the engine does about **8x the work per core**,
and that ratio is spread over ~50 mechanisms, none above ~2% (top host row 4.4%; two shipped
changes cut their microbenchmarks 35-66% and moved the boot 0.00%).

**Parallelism, fully realised, does not close it.** The boot runs at 1.07 cores against
java's 3.16, so the ceiling looks like 3x. It is not, because of the phase split: start ->
`Environment:` is 30 s of `Bootstrap` and static initialisation, which is serial by the
language's own ordering rules, and only the 18 s reload phase fans out. Perfect parallelism
there gives `30 + 18/3 = 36 s` — **-25%, from the single largest structural change available,
and still 5x off java.**

**Nothing measured offers the other 8x.** Eleven lines were closed with numbers this session:
the GIL, green daemons, the deferral lane, dispatch misses (7 M removed, 0.00%), the native
bridge routing, key derivation, field interning, `entrySet` materialisation, running the real
JDK bytecode (5.7x SLOWER and it does not link), codegen (1.1 ns/call vs HotSpot's 0.8), and
the object representation (arena flags compose to -0.20%).

So the honest conclusion is not "keep optimising" and not "one more lever". It is that this
engine's architecture — a Rust runtime servicing guest operations that HotSpot compiles and
inlines, with 8.2% of the boot inside compiled code against 91.8% in the runtime — caps out
near 7x slower than HotSpot on this workload. Closing that gap is a different engine, not a
fix to this one, and the decision to build one is the owner's, not a change to be slipped in
as another increment.

What is worth doing inside the current architecture, in order, and none of it reaches the
goal: repair `RAVA_ARENA_OVERRIDE` and `RAVA_ARENA_SYNTH` the way `MAPS` was repaired (they
hang today); raise the fraction of the boot inside compiled code, which is the only quantity
that tracks the gap; and keep every timing of this codebase paired, interleaved, in a
configuration that can see the mechanism, with the four gates beside it.

### `RAVA_ARENA_OVERRIDE` diagnosed: not a hang, a >=14x slowdown, and the cause is named

ChunkFill under the flag was killed at 200 s, then at 600 s, against a 41.6 s baseline
(`FILL_MS=7807`, hash OK). Sampled while it ran, both tables are NORMAL and FLAT — the guest
side is ordinary VoxelShape/StateHolder work (top method 0.6%) and the host side is the usual
`jit_call_body` 3.6 / `mi_malloc` 3.2 / `invokevirtual` 3.2 spread. No hot spot, no loop.

A uniform slowdown on unchanged work with an unchanged profile shape means the objects moved
to a representation whose access is slower everywhere. That is exactly what the flag does:
`pure_value_collection_override` arena-allocates a native-collection subclass that supplies its
own accessors — `CubePointRange` is its named target — and `fu_fastpath`'s arena arms
(`DImmList`, `DArrList`, the fastutil ints/doubles names) do not list those subclasses, so
every `getDouble` that used to be served off the native backing now takes the general path.
The flag's own comment anticipated the shape of this: "a wrong relaxation silently no-ops a
real native accessor".

So repairing it is not a bug hunt but a coverage question: an arena-allocated overriding
subclass needs a fast path of its own, or must not be migrated while its accessor is hot.
Worth stating because it is the opposite of the `MAPS` repair — there the flag was WRONG and
looked fast; here it is CORRECT (no wrong answers, just no output in 14x the time) and is
simply not viable in its current form.

### The complete arena-family map

Previously this family was six opt-in flags with no characterisation. It is now mapped, each
member measured for correctness AND speed, and the maximal correct combination measured too.

    flag                    boot   4 gates   19 probes   paired CPU A/B
    RAVA_ARENA_MAPS         OK     OK        OK          -2.53%  3/5  (at the floor)
    RAVA_ARENA_RECORD       OK     OK        -           -
    RAVA_ARENA_ENUM         OK     OK        -           -
    RAVA_ARENA_NATIVE_NEW   OK     OK        OK          -1.61%  2/5  (below the floor)
    RAVA_ARENA_SYNTH        OK     FAIL      -           void
    RAVA_ARENA_OVERRIDE     OK     FAIL      -           void (>=14x slower on ChunkFill)

    MAPS + NATIVE_NEW                        OK          -0.20%  2/4
    MAPS + RECORD + ENUM + NATIVE_NEW  OK    OK          +1.21%  1/4   <- maximal CORRECT set
    SYNTH + NATIVE_NEW + OVERRIDE            HANGS
    all six                                  HANGS

`MAPS` is gate-clean only because it was repaired this session (four correctness defects).
`RECORD` and `ENUM` were already clean and had never been checked. The two that fail were
opt-in for exactly that reason, and `OVERRIDE`'s failure is a coverage problem, not a wrong
answer.

**The maximal correct arena configuration measures +1.21% with one win in four.** So moving
object families off the handle heap — the representation hypothesis in its strongest testable
form — buys nothing on this boot, and the line is closed with a map rather than an opinion.
Note also that every attractive number this family produced (+48%, -8.82%, -3.54% at 4-of-4)
came from a configuration that fails the gates.

## The one opening found for parallelism, specified

`[PC-DEFER] offered=7 | DEFERRED=7` on a whole boot: the lane refuses nothing, it is asked
almost nothing. `gilfree_cf::defer` has ONE call site — the `supplyAsync` arm at
`reflect/futures.rs:218`. `runAsync` does not offer, and none of `thenApplyAsync`,
`thenComposeAsync`, `thenAcceptAsync`, `thenCombineAsync` do: they all route to
`futures_glue::cf_chain`, which for a COMPLETE source runs the body inline
(`run_cb_protected`). MC's `SimpleReloadInstance` and `SimplePreparableReloadListener` are
built on exactly those, so the parallelisable work of the 18 s reload phase never reaches the
only mechanism that can defer it.

The hook is `cf_chain`'s complete-source branch, and the semantics are sound: an `*Async`
body may run on any thread by the `CompletableFuture` contract, and a complete source means
the value is already known — structurally identical to `supplyAsync`.

**The blocker is the lane's task shape, not the seam.** `LaneTask { task, future }` carries a
SUPPLIER: `plan_meta` builds the call vector from the lambda's captures and the SAM takes no
arguments. A `thenApplyAsync(fn)` body is `fn.apply(v)` — one argument, the source's value,
which the task cannot carry. So the increment is:

1. `LaneTask` gains an optional SAM argument; `plan_meta`/`pack_and_claim` append it to the
   call vector after the captures, exactly where `invoke_lambda_parts` puts SAM args today.
2. The argument must be GC-rooted for the region and must live in the task's nursery slice,
   like every other value a worker touches (`push_call_roots` is the existing shape).
3. `cf_chain` offers only when the source is complete AND the call was an `*Async` variant —
   which means plumbing an `is_async` flag, since `cf_chain` currently receives only `kind`
   ("apply"/"accept"/"compose") and cannot tell `thenApply` from `thenApplyAsync`.
4. `thenCompose*` stays inline regardless: exempting it from the drain broke datapack loading
   once already, and its function builds the reload graph rather than computing a value.

Ceiling, from the phase map: the reload is 18 s of a 48 s boot and the bootstrap 30 s is
serial by the language's own `<clinit>` ordering, so perfect fan-out gives `30 + 18/3 = 36 s`,
about -25%. That is the largest single structural change still available and it does not reach
the goal by itself.

## The parallel line, taken as far as measurement supports

The lane's task shape no longer refuses one-argument bodies (`LaneTask::args`), and `*Async`
continuations are offered at both possible hooks — chain creation and callback firing. Both
were measured, and the target moved twice:

1. **The lane is not missing work.** `[PC-DEFER] offered=7 | DEFERRED=7`: seven tasks a boot,
   all accepted, zero refusals. MC's reload is `supplyAsync(prepare) -> thenCompose(barrier)
   -> thenAcceptAsync(apply)`, and the parallel half — `prepare` — was already the one thing
   offered. Adding the `*Async` hooks moved offers 4 -> 5, because chaining onto an
   ALREADY-COMPLETE future is rare by construction and `thenAcceptAsync` is void.

2. **Two limiters, both downstream of admission, both now quantified.**
   * *Batch fragmentation*: the seven arrive in FIVE batches (`n=3, 1, 2, 1, 1`) because main
     drains per listener at its `thenCompose`. Even with every task completing, that caps the
     region near 2-wide, not 4.
   * *Aborts*: one of five regions completes (`run#3 keep=true`), up from zero before this
     session's two fixes.

3. **The dominant abort is now named.** `first=java/lang/InternalError: direct miss` on three
   of four aborting runs — a worker reaching a direct-call site whose target is not compiled,
   which it cannot resolve off-GIL. The coordinator's pre-pass reports `warm-compile 9/25`:
   sixteen of twenty-five general-path callees fail to force-compile before the region opens,
   and each is a live abort. That, not the admission rules, is what to fix next.

4. **A tunable that visibly matters.** `RAVA_GILFREE_SLICE_KB` defaults to 192 KB against a
   ~236 MB pool. Raising it to 16 MB took the largest task from 145,691 callbacks to 289,058
   before it aborted — twice the work completed off-GIL — and changed its failure from a
   shared-field deopt to the direct-miss above. Not a win on its own (the region still
   aborts), but the default is clearly too small for the tasks the reload actually submits.

Ceiling unchanged and worth repeating: the reload is 18 s of a 48 s boot and the 30 s
bootstrap is serial by `<clinit>` ordering, so even perfect fan-out is `30 + 18/3 = 36 s`.

## The causal chain, closed

Chasing the lane's dominant abort ended at the same architectural fact everything else did,
reached independently for the third time:

    java.util served by Rust natives
      -> those classes have NO BYTECODE
      -> `force_compile` refuses them at class load (18 of 30 warm targets)
      -> the coordinator cannot pre-warm them
      -> a worker reaching one has an unresolved direct-call target
      -> it cannot resolve off-GIL  (`vmref.rs:137`)
      -> `InternalError: direct miss`, region aborts   (3 of 4 aborting runs)
      -> the reload runs serially
      -> 1.07 cores

The same fact wears three other faces already measured here: `dispatch.rs:298`
(`cls.starts_with("java/") => NeverCacheable`, 19.4 M PIC refusals), the JDK-bytecode
experiment (real `java.util` runs 5.7x SLOWER and does not link), and the wall buckets
(NATIVE_BRIDGES 26% + JIT_CALLBACK 25%, with RIP sampling putting 8.2% inside compiled code
against 91.8% in the runtime).

Two ways out, both real projects, neither a tuning:

1. **Let workers call natives.** The channel exists — `jit::vmref::vm_shared()` hands any
   thread a read-only `&Vm` with no abort — and the safety argument is available: inside a
   region main is parked and no collection runs, so the heap is frozen and a READ-ONLY native
   races nothing. The cost is that the native surface takes `&mut Vm` throughout, because
   natives allocate and mutate; a read-only variant is a signature change across hundreds of
   them. A narrow start exists in `fu_fastpath`'s read-only ops (`size`/`get`/`isEmpty`).
2. **Give java.util compilable bodies.** Refuted in its obvious form this session (the real
   JDK's bytecode is slower here AND needs a `jdk.internal.*` surface we do not have), so this
   means writing bodies the JIT can compile, not importing them.

Everything shipped this session is orthogonal to that choice and holds either way: the lane
now completes one region where it completed none, its tasks can carry SAM arguments, and every
number above is reproducible from censuses that did not exist yesterday.

### The last link, traced to the guard

One correction to the "let workers call natives" option, found by reading rather than
guessing: the locked-exec envelope is NOT missing and NOT off. `worker_locked_exec_enabled()`
is implied by `rings()` (its own comment: "A lane that cannot work without a second env var
must turn it on itself"), `rava_dispatch_miss` takes `resolve_lock_acquire()` UNCONDITIONALLY
for workers, and it uses `vm_ref_worker_sanctioned()`. A worker can therefore RESOLVE under
the lock today.

What it cannot do is EXECUTE a callee with no compiled body — the comment at
`callbacks/dispatch.rs:59` says so exactly ("an uncompiled-callee continuation aborts inside
`jit_call`'s worker guard before any publication"), and the abort lands on the named sites
(`vmref.rs:137`, `fields.rs:276`). Since `java.util` is served by natives and has no compiled
body, that is the common case, not the corner.

So the project is narrower than "a read-only native surface": it is *may a worker EXECUTE a
native under the exclusive resolve lock it already holds?* Main is parked, no collection runs
in the region, and the lock admits one worker at a time — the same serialisation the envelope
already grants for resolution. What makes it a project rather than a flag is that natives
mutate SHARED state (not only the task's own slice), and nothing today distinguishes the ones
that do from the ones that do not.

### Option 1 closed with a number: the read-only surface is inherently ~25%

Widening `fu_read_shared` to `java/util/ArrayList` was sound, shipped, and moved the aborts by
nothing. Classifying the `[WARM-NOCLASS]` population says why, and closes the option:

    BasicFileAttributes#isRegularFile   pure read          ELIGIBLE
    ArrayList#get(I)                    pure read          ELIGIBLE (shipped)
    [arena-string]#endsWith             pure read          ELIGIBLE
    HashMap#forEach                     runs guest code    no
    ArrayList#iterator                  allocates          no
    HashMap#values / #keySet            allocate           no
    TreeMap#<init> / #putAll            allocate / mutate  no
    StringBuilder#<init> / #toString    allocate           no
    Path#relativize                     allocates          no

**Three of twelve.** The shared path takes `&Vm`, and allocation needs `&mut` because it
mutates the heap's shared structures (handle table, array spine) — the lane's own worker
allocation goes through pre-reserved slots and the exclusive machinery, not this path. So
"let workers read natives" can cover at most a quarter of the population that aborts the
region, and the rest allocates, mutates, or re-enters guest code by its nature.

That closes option 1 as a route to parallelism. What remains is option 2 — java.util bodies
the JIT can compile — which is the same conclusion every other line reached this session, and
it is a different engine rather than a change to this one.

## The regime changed: the tiering loop was open, and everything above was measured through it

`dispatch_resolve_cacheable_cls` ended with `_ => NotYet` and did nothing to make a later call
find the callee compiled. A method reached ONLY from compiled code accumulates no interpreted
entries, so entry-count tiering never fires, so `jit_cache` never gains a `Compiled` entry, so
the arm answers `NotYet` again — forever. Closing it (`e52c4ef0`, default on, kill switch
`RAVA_NO_DISPATCH_TIER`, hotness bound `RAVA_DISPATCH_TIER_N=64`) took

    virtual mono   322 -> 8 ns      interface mono 310 -> 8 ns
    virtual array  342 -> 10 ns     interface poly 306 -> 9 ns

and the product boot from **48.94 s wall / 52.45 CPU to 36.84 / 40.3** — -25% and -23%, with
`Done` 0.45 -> 0.40. Two interleaved A/B series read -11.71% (DISJOINT, 4/4) and -12.02%.

**That invalidates the profiling this document is built on.** Every host profile above was taken
with machine-code PIC coverage at 0.0% — every instance call went to the runtime — so "8.2%
inside compiled code against 91.8% in the runtime" was in large part this defect, not the
architecture. The boot's dispatch-table hit rate is now 3.8% -> 13.2% and arena-receiver misses
are down 63%. The refutations of *specific mechanisms* (dispatch misses are cheap, the arena
flags compose to nothing, real JDK bytecode is slower, the lane is not starved) stand on their
own A/Bs; the *conclusion* drawn from them — "the gap is architectural, it needs a different
engine" — does not, and is withdrawn.

### Two method lessons this cost

**A microbenchmark's sink is part of the measurement.** `CallShapes` first read `virt ref-ret
448 ns` and `new 238 ns`; profiling showed 53% of both was the STATIC-FIELD sink
(`putstatic` 24.8% + `jit_putstatic` 9.4% + `HashMap::insert<String,..>` 8.6% + `get_or_load`
10.6%). With a non-static sink: 16 ns and 7 ns.

**A microbenchmark can measure the exact opposite of the boot.** Removing the `String`
allocation from `putstatic` took `static put int` 260 -> 185 ns and the boot measured the
ALLOCATING form **2.81% faster** (n=4, `-Jit`, 3/4 pairs): a static is written once, in
`<clinit>`, so the borrowed `get_mut` probe misses and the write pays two lookups. Reverted.

### The operation sweep, and what is left

`bench/OpCost.java` compares operations against the JVM the way the tiering defect was found.
Most of the engine is healthy — array get/put 1-5 ns, long/double arithmetic 0-4, checkcast 3,
interface default method 7. The outliers, after swapping `tlcache`'s SipHash for FxHasher
(`987ac47c`; instanceof 67->45, String.length 133->91, charAt 235->171):

    lambda apply  871 ns (java 1)   <- THE NEXT TARGET
    synchronized  249 ns (java 17)
    String.charAt 171 ns (java 0)
    try/catch     175 ns (java 1)

**The lambda is the big one and it is the same shape again.** `[PIC-COV]` on a bare
`IntUnaryOperator` call: 2,200,000 dynamic calls, **machine-code PIC coverage 0.0%**, 100%
falling to `rava_dispatch_miss`, and `λ-PIC fills: deep=0 leaf=0, refused: shape=0 rooting=0
frame=0` — the λ-PIC is never consulted at all. It is gated on `RAVA_LAMBDA_PIC`, **opt-in,
default OFF**, and turning it on changes nothing (913 vs 938 ns), so it does not fire even when
armed. The lambda BODY does compile (`jit entries=22`); the 871 ns is the seven-hop path to it:
`rava_dispatch_miss` -> `jit_call` -> `invokevirtual` -> `apply` -> `invoke_lambda_value` ->
`invoke_lambda_parts` (`resolve_method`, `Arc::from(class_name)`, `super_chain`) -> `invoke_idx`
-> frame -> compiled body. MC's reload phase is codec decoding, i.e. lambdas end to end.

### The lambda path, diagnosed to the line

Three flags tested on `bench/LamProf.java` (a bare `IntUnaryOperator` apply, 871-1020 ns against
the JVM's 1): `RAVA_LAMBDA_SITE_CID`, `RAVA_DISPATCH_TABLE`, `RAVA_LAMBDA_PIC`, alone and
together. All read 881-1020 ns — noise. None of them is the missing piece.

The reason is at `vm/dispatch.rs:208`. The λ-PIC's classification and its fill are both

    if let Value::Ref(h) = jit::unbox_ref(self, recv, "") { if self.heap.is_lambda(h) { ... } }

i.e. **HANDLE lambdas only**. An arena lambda (`Value::Obj`) is never counted and never filled,
which is exactly what `[PIC-COV]` reports: 2,200,000 misses, `lambda-receiver misses 0`,
`λ-PIC fills: deep=0 leaf=0, refused: shape=0 rooting=0 frame=0` — never consulted, not refused.

And the arena arm was not merely forgotten. The λ-PIC is IDENTITY-keyed, and an arena object's
identity is its address, which the moving collector changes — so identity cannot be its key. The
stable alternative already exists (`AL_SITE_SLOT`, which `emit_table_probe` reads under
`site_cid_enabled`, and which `dispatch_table::note` mirrors via `lambda_table_key`), but the
classification and fill above never route an arena receiver to it.

So the lambda's 871 ns is the seven-hop host path — `rava_dispatch_miss` -> `jit_call` ->
`invokevirtual` -> `apply` -> `invoke_lambda_value` -> `invoke_lambda_parts` -> `invoke_idx` ->
frame -> compiled body — taken 2.2 M times because no cache on the way can hold an arena lambda.
The body itself compiles fine (`jit entries=22`). That is the next piece of work, it is bounded,
and it is the same "ARENA TWIN" shape `gilfree_cf/plan.rs` already applies elsewhere.

### …and the exact blocker under it, with the hazard that keeps it a project

Traced one level further. `rava_dispatch_miss` reads `class_id` from the arena receiver's
header and hands it to `dispatch_resolve_cacheable`. For an arena LAMBDA that id is the
SYNTHETIC one shared by every lambda of the same capture shape, its name starts with `$`, and
`dispatch_resolve_cacheable_cls`'s clause 1 rules it `NeverCacheable` — writing a permanent -1.
So every lambda call is a proven-uncacheable miss by construction.

The receiver does carry a stable, correct key: `AL_SITE_SLOT` names the IMPL, which is a real
class with bytecode that compiles fine. Both sides of the dispatch TABLE already speak it —
`emit_table_probe` computes `site + LAMBDA_KEY_BASE` under `site_cid_enabled`, and
`dispatch_table::note` mirrors it through `lambda_table_key(p, class_id)`.

**The hazard is why this is not a one-liner.** The 4-way PIC is keyed by `class_id` alone, and
all lambdas of a capture shape share it — so returning `Compiled(fptr)` would let a DIFFERENT
lambda arriving at the same site hit the entry and enter the WRONG body. That is exactly what
the `$` clause is protecting. A correct fix has to split the verdict: keep the 4-way PIC
refusing, and fill only the site-keyed TABLE. `dispatch.rs` currently derives one `fill` and
hands it to both.

That is the shape of the work: a per-representation verdict rather than one, with the
lambda arm resolving through `lambda_site_get(arena_lambda_site_idx(p))` instead of
`class_name(class_id)`. Prize: a lambda apply is 871 ns against the JVM's 1, and MC's 18 s
reload phase is codec decoding — lambdas end to end.

### The last link, and why it is a project and not a patch

The fill API exists and is exactly right: `dispatch_table::note_serve(cache_ptr, recv, fill)`
takes the RECEIVER, computes `lambda_table_key` itself, and writes the site-keyed table —
leaving the class-keyed 4-way PIC untouched, which is the split the hazard above demands.
(Writing -1 into the PIC would not do: the emitted probe treats any non-zero way as a hit and
never reaches the table, so a lambda's PIC slot must stay EMPTY, i.e. the `NotYet` behaviour.)

What cannot simply be handed to it is the `fill`. The site names the IMPL — a static method
whose frame begins with the CAPTURES — while the call site supplies only the receiver and the
SAM arguments. A raw impl pointer is therefore not callable from the emitted probe; it needs a
thunk that unpacks the capture cells out of the lambda object and then tail-calls the impl.
That thunk exists (`jit/emit/cap_thunk.rs`, installed from `jit_direct_call`'s `DirectProbe::Hit`
arm) but is not reachable from this path.

So the complete piece of work is: on an arena-lambda receiver, resolve the impl through
`lambda_site_get(arena_lambda_site_idx(p))`, obtain or build its capture thunk, publish that
pointer with `note_serve`, and leave the 4-way PIC empty. Every part exists; none of them is
wired to the others. Shipping it blind would risk a lambda entering another site's body, which
is the one failure mode the gates cannot see — so it wants a session with room to build the
probe first, in the order this document recommends: write the probe, confirm it FAILS, fix,
confirm it passes, then the gates, then the timing.

## The cure, written out: emitted-inline collection access

`bench/OpCost2.java` prices the target and `bitset_get_inline` is the working precedent, so the
recipe can be written down exactly rather than described. Every piece already exists:

**The pattern** (`jit/emit/collections.rs::emit_bitset_get_inline`, ~90 lines):
1. Guard: `recv >> 40 == 1` (REF_TAG) and index >= 0 → else slow.
2. Guard: `emit_heap_ref_class_key(b, array_desc_cell, recv, slow)` equals a baked class word,
   and the baked word is non-zero → else slow. (Non-zero is what makes it self-bootstrapping:
   until the word is published every call takes the slow arm, which publishes it.)
3. `native_getfield_ref(b, env, array_desc_cell, recv, env.<field>_fid)` — read the backing
   field BY ID, inline, no callback.
4. Check the cell is `ARRAY_TAG`, then `load_array_desc(b, array_desc_cell, cell)` → `(data,
   len)`, bounds-check, load at `data + i*8`.
5. Slow arm: spill for GC, build args in `env.args_slot`, call an FFI FuncRef — `env.fu_invoke`
   already serves `ArrayList.get/size/isEmpty` and is the right fallback.

**The three supporting pieces**, each with its precedent:
* Field id: `driver.rs:110` resolves `bits_fid` by scanning the bytecode for a `BitSet.get`
  site and calling `intern("java/util/BitSet", "$bits")`. The same scan for
  `java/util/ArrayList.get (I)Ljava/lang/Object;` gives `$elems`.
  (`intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>)` is already a parameter.)
* Baked class word: `jit/runtime/ic.rs::BITSET_CLASS_WORD`, published once by
  `vm/jit_collections.rs::jit_bitset_recv` as the first word of `heap.intern_class(..)`.
* Emit site: `jit/emit/ops/invoke_virtual.rs:67`, next to the existing BitSet arm, guarded by
  the same "no live reference operand below the receiver" stack condition.

**And why this is not being started at the end of this session.** `ArrayList#get` alone is
1,569,860 boot calls at 187 ns = 0.29 s = **0.8%, under the floor**. The 5-10% prize is the
whole iteration family — `Iterator#hasNext` 4,039,549, `#next` 3,250,504, `Map$Entry#getKey`
2,119,290, `#getValue` 2,117,944 — which is four emit functions, four guards and four slow
arms. Building one of them would produce something unmeasurable; building four is a codegen
change that wants a session of its own, and the gates plus `bench/LambdaSiteSem.java` plus the
19 probes are the net it should be built against.
