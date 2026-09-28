# Why a capturing lambda cannot be served from the machine-code PIC (yet)

> ⚠**2026-08-25 — ONE PREMISE OF THIS DOCUMENT IS STALE.** Below it says the arena-lambda
> representation "is default-OFF and, per the value-model plan, currently crashes a real boot".
> **It no longer crashes.** `RAVA_ARENA_LAMBDA=1` boots the product configuration clean —
> `wall 49.6 s, Done 0.500`, no fault — because the arena guard that caused it was fixed
> (the committed-extent table; the guard used to trust reserved-but-uncommitted VA). So the
> machine-code lambda PIC is no longer blocked by a crash.
>
> It is still worth nothing, and that has now been measured rather than assumed. On
> `bench/ShapeOr.java`, the workload this document was written against, all three arms are
> bit-exact and indistinguishable:
>
> | arm | HOTNS |
> |---|---|
> | baseline | 13.908 s |
> | `RAVA_ARENA_LAMBDA=1` | 13.953 s |
> | `+ RAVA_LAMBDA_PIC=1` | 14.047 s |
>
> `java` does the same work, same hashes, in **0.256 s — 54×**. The λ-PIC census's
> `ADMITTED = 0` is therefore not explained by the representation either; the gate refuses these
> receivers for some other reason, and that reason is the next thing to find, not the crash.


Recorded 2026-07-25, commit 3417063, measured on `bench/ShapeOr.java`.

## The premise that motivated the hunt, and where it was wrong

The `RAVA_JIT_PIC_PROBE` census on ShapeOr reads:

```
dynamic calls out of compiled code = 13889279
  machine-code PIC hit                9792602 (70.5%)
  fell to Rust host                   4096677 (29.4%)
      PIC miss -> rava_dispatch_miss  3548627 (25.5%)
        lambda-receiver misses        3154796 (22.7%)  directable 1411982
[HEAPPIC-LIVE] cache fills=38 never-cacheable=3544456
```

`never-cacheable=3544456` reads like a permanent marking that condemns lambda sites to
the full interpreter gauntlet. It is not. That counter is incremented by
`Vm::heappic_fill`'s fast negative bail — a `$`/`java/` class-name prefix test that
DECLINES to fill the site cache and writes nothing. No negative entry is stored, so it
re-bails per call; it is a tally of declined fills, not a state.

What actually serves those 3.15M calls is the Rust direct-call path: the same run
reports `direct=3761357`, i.e. 92% of all host falls are resolved by
`Vm::jit_direct_call` against the per-site direct cache — already keyed on the lambda's
implementation (via the `Heap::lambda_fp` fingerprint), already prepending the captures.
Turning that arm off is the proof: `RAVA_NO_CAP_LAMBDA=1` costs +58.5% and
`RAVA_LAMBDA_DIRECT=0` costs +73.4% on the probe.

So the lambda gap was never "no cache". It was "the cache key is expensive and the fall
into Rust is unavoidable". `RAVA_LAMBDA_IMPL_KEY` (docs/lambda-impl-key.md) fixes the
first half for -24%. The second half is below.

## The blocker for a machine-code hit arm

A machine-code PIC hit is `call_indirect(fptr, bufaddr, ctx)`. For a lambda the callee
is the SAM implementation, whose locals are `[captured_0 … captured_{n-1}, sam_arg_0 …]`
while the call buffer holds `[receiver, sam_arg_0 …]`. So the hit arm must materialise
the captures into the buffer on every call — they differ per receiver, which is the
whole reason one PIC entry can serve many receivers.

The captures live in `heap::Lambda { captured: Vec<Value> }`: a heap `Vec` of the
24-byte tagged `Value` enum, whose reference arms additionally need `box_ref` re-homing
and a GC forward barrier per call. Cranelift cannot decode that — reading it is a
discriminant switch per slot plus a call into the box table. The existing Rust arm does
exactly this work in `Vm::jit_direct_invoke`, and it is not the expensive part; the
expensive part is getting to it.

Consequences:

* **Capturing lambdas (1.74M of the 3.15M lambda falls here) are blocked** until a
  closure's captures are FLAT, GC-traced cells readable by generated code. That is the
  `RAVA_ARENA_LAMBDA` representation (`nextgen.rs`, `AL_HDR_CELLS` + inline capture
  cells with a per-signature `ref_mask`) — which is default-OFF and, per
  [[rava-value-model-rewrite-plan]], currently crashes a real boot. The machine-code
  lambda PIC is DOWNSTREAM of that representation work, not independent of it.
* **Non-capturing lambdas (1411982 "directable" falls) are not blocked by captures**,
  and `RAVA_LAMBDA_PIC` already emits the codegen for them (`emit_lambda_key_select` →
  `emit_refpic_probe` → `emit_lambda_shift_adjust`). It is INERT on this workload —
  census byte-identical, time +0.8% — because `Vm::lambda_pic_fill` only caches entries
  with `entry.self_reg`, i.e. callees compiled with the Deep-IC v3 self-registering
  prologue. These lambda bodies are tiny and GC-LEAF, so they are compiled WITHOUT that
  prologue and every candidate is refused; `LAMBDA_CLASS_WORD` is never published and
  the emitted select can never fire. Admitting a LEAF entry (`fptr|2`, no bit 0 — the
  `refhit` arm already honours the SHIFT marker) would require `lambda_pic_fill` to
  prove GC-leafness of the impl body the way `dispatch_resolve_cacheable_cls` does,
  since the leaf arm performs no caller-side `cells_push`. That is the next contained
  step if the ~1.4M directable falls are judged worth it — it is a fill-admission
  change, not new codegen.

## The one-line summary

Impl identity IS sound and IS available to machine code with one extra dependent load
(`slot → Object → Lambda → site`). It is the CAPTURE ACCESS, not the key, that keeps a
capturing-lambda call site off the machine-code arm.

## UPDATE 2026-07-25 — the capturing half is unblocked, one layer below machine code

Two things above were wrong or have since been fixed, and one is now built.

**The `Throw(Ref(0))` boot failure was never arena-lambda's.** Commit 7097ff1 root-caused
it to `gil::set_worker()` being latched for an executor thread's whole life while three
allocation sites read it as "am I gil-free with a pre-reserved segment", and retired the
signature categorically. So the sentence above — "currently crashes a real boot" — no
longer rests on that evidence. Re-measured on this build, `RAVA_ARENA_LAMBDA=1` passes
ChunkFill bit-exact and shows no value divergence anywhere in the bench corpus
(DataPackJson 6351 files fail=0 hash 14e8ee5a, DecodeProbe hash a17dbfa0, Collections /
LambdaDedup / GsInline / AsyncReduce / ParReduce / ForkEach / Dispatch byte-identical).

**What DID corrupt a reload was the consumer surface, not the representation.** About a
dozen `matches!(v, Value::Ref(_))` gates predate the arena representation and read an
arena-lambda as "not a function", then silently drop the guest's work — no error, just a
differently-shaped world downstream. The sharpest one is `nio.rs` `Files.find`, whose own
comment records the previous time it was skipped: "listResources abort with no resources
(every datapack registry came up empty)". They now route through `Value::is_fn_value()`,
which admits `Ref` plus REGISTERED arena-lambda classes only — deliberately not
`Ref | Obj`, since `Value::Obj` is the ordinary arena object too and the broad form would
change default behaviour.

**The capture-copy is now emitted — in the Rust direct-call arm, not yet in Cranelift.**
`Vm::jit_direct_resolve` / `_probe` / `_invoke` accept an arena-lambda receiver as a third
representation. The guard is the interned SITE INDEX (`nextgen::AL_GUARD_BASE`), which is
what makes this work at all: a capturing closure's address is fresh per instantiation and
its arena class is shared by every site with the same capture kinds, so only the site is
stable. That is the same property `RAVA_LAMBDA_IMPL_KEY` buys for handles, here for free
and as an identity rather than a hash. Captures are read straight from the inline cells
(`AL_HDR_CELLS + i`) — no `Vec<Value>` clone, no box-table walk, no per-call malloc.

Measured on ShapeOr, SUM bit-exact (`3cfa327f01e0c901`) in both arms:

| probe | OFF | ON | delta |
|---|---|---|---|
| 200 passes / 50 warmup (canonical) | 25834 ms | 22108 ms | **-14.4%** |
| 40 passes, ABBA (`shapeor_flag_ab.ps1 off,on,on,off`) | 4880 ms | 4302 ms | **-11.8%** |
| lambda-receiver host falls (`RAVA_JIT_PIC_PROBE`) | 3154796 | 1411982 | **-1742814** |

The fall count is the mechanism, and it lands exactly where predicted: the 1.74M capturing
population named above is gone, and the residual 1411982 is precisely the "directable"
non-capturing set that blocker #2 still owns. Total dynamic calls (13889279), machine-code
PIC hits (9792602) and total host falls (4096677) are byte-identical between arms — as
they must be, since no codegen changed. What changed is what a fall COSTS: a capturing
arena-lambda used to re-resolve through the full general dispatch on every call and now
hits a cached direct entry.

ChunkFill gates `8790f04ace726ecf` in both arms. Its SETUP is still slower with the flag on
(81764 vs 117420 ms), but that pair is unpaired and single, and this metric's own spread
across today's runs (81124 / 94474 / 95922 / 101446 OFF-and-ON) covers the gap — it is not
evidence either way and a proper interleaved A/B is still owed.

## UPDATE 2026-07-26 — blocker #2 is REMOVED, and it prices the whole arc at ~100 ns/fall

**Why `self_reg` was required, and why a LEAF entry does not need it.** The requirement was
never about the lambda; it was about ROOTING. A machine-code PIC hit is a bare
`call_indirect` with no Rust wrapper, so nobody performs `jit_direct_invoke`'s
`frames::cells_push` for the callee's frame. A non-leaf callee can reach a safepoint, and a
collection that runs inside it must be able to find the references in that frame — hence
`self_reg`, the Deep-IC v3 prologue that makes the callee register its own buffer. A
GC-LEAF body is the other way out of the same obligation: `is_gc_leaf` refuses every
invoke, allocation, `ldc`-of-a-ref, `athrow`, monitor and static access, so no collection
can occur inside it at all — there is no frame to root, and no caller reference can go
stale across it. That is not a new contract: it is exactly the one `heappic_fill`'s
class-word leaf entries (`fptr|0`, served by `refhit` with no spill and no reload) have
shipped on by default since 23.4b. A lambda leaf entry adds only the identity key and the
SHIFT, both already decoded by `emit_lambda_shift_adjust`.

`!self_reg` alone does NOT imply leaf (a non-leaf body compiled by the folded/parallel
emitters also lacks the prologue), so `DirectEntry` now carries an explicit `leaf` fact
computed at resolve from the callee's own bytecode. `lambda_pic_fill` admits either shape
and marks the arm in the cached fptr: `|1|2` deep, `|0|2` leaf.

**Measured on ShapeOr (40 passes), SUM bit-exact `fd51eaa48cdef501` in both arms:**

| probe | OFF | ON | delta |
|---|---|---|---|
| dynamic calls out of compiled code | 13889279 | 13889279 | 0 |
| machine-code PIC hit | 9792602 (70.5%) | 11204579 (80.6%) | **+1411977** |
| fell to Rust host | 4096677 | 2684700 | **-1411977** |
| lambda-receiver falls / directable | 3154796 / 1411982 | 1742819 / **5** | -1411977 |
| λ-PIC fills | — | **leaf=5 deep=0** | — |

Every directable fall is gone, five cache fills serve all 1.41M of them, and the arm split
is the one the leaf hypothesis predicted: **zero** candidates were `self_reg`, which is why
the pre-change fill admitted nothing at all.

**And the wall says the premise of the arc was wrong by an order of magnitude.** Removing
1,411,977 host falls — 34% of every Rust-host fall on this workload, 10.2% of all dynamic
calls — is worth **-2.3%** (min-of-6 interleaved, 6209 → 6069 ms; medians are useless here,
the same config spans 16.1-27.8 s on this box). That prices a warm directable-lambda fall
at **~99 ns**, not the ~1.2 µs this document's opening census assumed. The falls were never
running the full dispatch gauntlet: 92% of them were already served by `jit_direct_call`
against a warm per-site cache, and what a machine-code hit removes is only the callback
boundary on top of it — which the λ-PIC's own codegen tax (an extra global load + select at
every refpic site, +0.8% measured inert) very nearly cancels.

ChunkFill gates `8790f04ace726ecf` with the flag off and on, crossed with `RAVA_ARENA_LAMBDA`
off and on (four configs). `cargo test -p rava-interp --release --features next-gen-gc`: 368
passed. Default stays OFF pending the boot A/B.

## The capturing arm (#1) is FEASIBLE and PRICED — and the price does not justify it

Censused on the same run (`RAVA_ARENA_LAMBDA=1`, 1,740,702 capturing arena-λ calls):

* **Which hit arm could serve them: `self_reg=1740702`, `leaf=0`, `neither=0`.** All of them
  would take the rooted DEEP arm. That arm is safe for the capture copy specifically because
  `DeepShape::args_slots` makes the prologue zero only `[args_slots .. frame_len)`, and
  `args_slots` counts the IMPL's arguments — captures included — so a copy written at
  `[0..cap_n]` is not wiped by the callee's own prologue.
* **What a capture cell holds** (17,912,354 cells, i.e. **10.3 captures per call**):
  prim 7475592, ref 6603245, array 3059740, raw arena ptr 773777, null 0, **arena-box index 0**.

So the copy is emittable in principle — `prim`/`arenaptr` are identity, `ref`/`array` are
`(cell >> 2) | TAG` — but it is NOT a memcpy: `decode_arena_cell` discriminates on the cell's
low two bits at RUNTIME, so every reference-kind capture needs its own test-and-select chain,
and the one outcome that is not a bit transform (`cell & 3 == 3` → `arena_box_get` → `box_ref`,
which may `box_push` into Rust-side state) can only be handled by bailing that call to the
miss arm. At 10.3 captures per call, replicated across 177 PIC-emitting sites, that is a
large amount of straight-line code guarding a population whose ceiling has now been measured:
blocker #2 moved 1.41M falls of the SAME kind, at the SAME sites, onto the SAME hit path for
-2.3%. Arm #1's 1.74M falls do carry more removable work per call (the Rust-side 10-cell
decode loop), but the boundary itself — the part a machine-code arm actually deletes — is the
~99 ns just measured.

The blocker on the capturing arm is therefore no longer "the captures are unreadable".
It is that **the thing being removed is worth ~100 ns/call**, and the arc's motivating
census over-priced it ~12x. Recorded rather than built.

A trap worth recording: the first cut asked `arena_lambda_of_cell(raw[0])` on EVERY direct
entry. On a static entry raw[0] is the first ARGUMENT, and a small aligned integer passes
an "aligned and under 2^40" shape test and then faults on the class-word read
(0xC0000005, both arms). The probe is now bounded by `is_arena_obj_addr` AND gated on
`shift`. Any future machine-code arm inherits this: the receiver-shape test that is
sufficient for a virtual site is NOT sufficient where the cell may not be a receiver.

## UPDATE 2026-07-29 — `RAVA_ARENA_LAMBDA` is a 78x REGRESSION on the gil-free lane

Both flags above were left default-OFF "pending the boot A/B", and that A/B was never run. Running
the correctness gates with them armed shows why it must not be run blind. All four gates are
BIT-EXACT — ChunkFill `8790f04ace726ecf`, RingScan / RingLane / RingLaneInstance `37539c6c375d0c3`
— and the two lane gates take **226-229 SECONDS** where the same binary without the flags takes
three.

Isolated, one flag at a time, RingLane(N=8) on the same build:

```
none          LANE_MS=3008     (wall  62.5s)
pic-only      LANE_MS=2939     (wall  42.8s)    <- harmless
arena-only    LANE_MS=235411   (wall 282.4s)    <- 78x
```

⇒ **`RAVA_ARENA_LAMBDA` is the regression; `RAVA_LAMBDA_PIC` is not.** The two must never again be
measured as a pair, and the ShapeOr numbers above (−14.4% / −11.8%) are single-threaded results
that say nothing about a lane-armed workload — which the real boot is.

⚠ HYPOTHESIS, not measured: the lane's workers cannot compile, so they depend on the serial
warm-compile pass having force-compiled every callee. If an arena-lambda receiver is not admitted
by that pass, every worker call interprets instead, and 78x is what an interpreted ring looks like.
The next step for anyone resuming this arc is to check `LANE_WARM_COMPILE`'s admission against an
arena-lambda receiver — NOT to re-run the boot A/B.

★ The bit-exact hashes are the trap here. Four green gates and a catastrophic slowdown coexisted
happily, because `LANE_MS` is documented as a correctness yardstick only. A gate that proves the
answer is right proves nothing about whether the mechanism is usable.

## Pre-registered before the first-ever boot A/B of `RAVA_LAMBDA_PIC`

Written before the result, so it cannot be fitted afterwards.

`RAVA_LAMBDA_PIC` serves the NON-CAPTURING ("directable") population only — that is what blocker #2
above removed, 1,411,977 falls on ShapeOr for five cache fills. The capturing population is a
different arm and is owned by `RAVA_ARENA_LAMBDA`, which this session has just measured as a 78x
regression on the gil-free lane.

The 2026-07-29 per-site attribution of the real boot puts the PIC-miss time here:

```
9.2%  IndirectMerger      @cp48  -> IndexMerger$IndexConsumer.merge(III)Z
6.0%  DiscreteCubeMerger  @cp41  -> IndexMerger$IndexConsumer.merge(III)Z
3.1%  Shapes              @cp141 -> BooleanOp.apply(ZZ)Z
```

**PREDICTION: the boot A/B will be null or near-null**, because the two dominant sites pass a
CAPTURING closure (the consumer writes into a shape the caller owns, so it must capture it), and
`RAVA_LAMBDA_PIC` cannot serve those. `BooleanOp.apply` is the opposite case — `BooleanOp`'s
constants are static finals, i.e. non-capturing — so if anything moves, it should be the ~3% site
and not the ~15% pair.

If that is what happens, the honest reading is NOT "the lambda arc is worthless". It is that the
boot's lambda time sits in the capturing arm, whose only implementation regresses the lane 78x —
i.e. the population is real and the mechanism that reaches it is not usable yet.

⚠ What would make this measurable rather than inferred: run the boot under `RAVA_JIT_PIC_PROBE`
with the flag on and read the λ-PIC fill count. Zero fills would prove the flag never engaged,
which is a stronger statement than a null wall delta — a null delta alone cannot distinguish "did
not engage" from "engaged and was worth nothing".

## ANSWERED 2026-07-29 — the prediction holds: the boot's lambda population is 89% CAPTURING

`bench/lambda_pic_fills.ps1` on the real boot, `RAVA_LAMBDA_PIC=1`:

```
[PIC-COV]  lambda-receiver misses   9,525,626 (1.8% of all dynamic calls)
[PIC-COV]      directable           1,057,855            <- 11%
[PIC-COV]  λ-PIC fills: deep=1 leaf=18   refused: shape=172,356
```

The prediction registered before the run was that the flag would barely engage, because the two
sites holding the boot's PIC-miss time (`IndirectMerger @cp48` / `DiscreteCubeMerger @cp41` →
`IndexConsumer.merge`) pass a CAPTURING closure. Confirmed:

* **8.47M of 9.53M lambda misses (89%) are capturing** — the arm this flag cannot serve.
* It made **19 fills** and refused 172,356 candidates on receiver shape. On ShapeOr five fills
  served 1.41M falls, so the mechanism works; the BOOT's population is simply the other kind.

### Why this matters more than the wall delta would have

The interleaved boot A/B was abandoned mid-batch (the box turned: one run read Done 42.1s / wall
114.6s against an ~84s norm, and the effect under test is order 1%). It could never have separated
this. The fill count is load-independent — a property of the code that executed — and it answers
the question the timing could not: **the flag did not engage, so a null would have said nothing
about the mechanism.**

### The state of the lambda arc, stated plainly

| arm | share of the boot's lambda misses | mechanism | status |
|---|---|---|---|
| non-capturing ("directable") | 11% | `RAVA_LAMBDA_PIC` | built, bit-exact, engages 19 times on the boot |
| capturing | **89%** | `RAVA_ARENA_LAMBDA` | built, bit-exact, **78x regression on the gil-free lane** |

So the population is real and concentrated, and the only mechanism that reaches the bulk of it is
unusable as it stands. That is a blocker with a measured size, not a refutation: the next step is
`LANE_WARM_COMPILE`'s admission of arena-lambda receivers — the hypothesis recorded above — and NOT
another boot A/B of either flag.

⚠ Do not read "1.8% of all dynamic calls" as "1.8% of time". Per-site attribution puts the whole
PIC-miss BOUNDARY at 20.4% of the serial window across 124 sites, of which the two merge sites hold
15.2%. Counting calls and pricing time are different measurements; this tree has the scar.

## The 78x lane regression has a NAMED mechanism (read from the code, not yet measured)

`Vm::jit_call_body` records the lane's warm-compile targets by CONCRETE RECEIVER CLASS, because
`force_compile` needs the class whose bytecode will actually run:

```rust
match jit::unbox_ref(self, raw.first()…, m.owner) {
    Value::Ref(h) => self.heap.class_of(h).map(str::to_string),
    Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(p)).map(…),
    _ => None,
}
…
crate::gilfree_cf::warm_note(&c, m.name, m.descriptor);
```

With `RAVA_ARENA_LAMBDA` on, a lambda receiver arrives as `Value::Obj`, and its arena class is
SYNTHETIC — shared by every site with the same capture kinds, as the 2026-07-25 update above
records. That name has no bytecode, so `force_compile` cannot resolve the method and the callee is
never compiled.

That poisons exactly the list warm-compile exists for. Its own doc says the whole purpose is one
symbol:

```text
BiomeSource.findBiomeHorizontal(...)  n=112  hold_ms=55028  max_ms=798
```

— a method invoked ~once per task, which therefore never reaches a call-count threshold, and which
runs INTERPRETED inside the lock if it is not force-compiled. One interpreted call owning 55s of a
77s batch is the same shape as LANE_MS 3008 → 235411.

⚠ **Read from the source, not measured.** What would settle it: run RingLane with
`RAVA_ARENA_LAMBDA=1` and read the `[GILFREE-RINGS] warm-compile ok/n` line — a collapse in `ok`
against the flag-off run confirms it; an unchanged `ok` refutes it and the 78x is something else.

THE FIX, if confirmed: for an arena-lambda receiver, warm-note the lambda's IMPL method (the SAM
target the site baked) rather than the synthetic arena class. The site index is already interned
(`nextgen::AL_GUARD_BASE`) and `jit_direct_resolve` already maps it to the impl, so the identity
needed is on hand — this is a key-choice bug, not a missing capability.

★ Note what this implies about the ShapeOr numbers that made arena-lambda look shipped-ready
(−14.4% / −11.8%): ShapeOr is single-threaded and never arms the lane, so it could not have seen
this. A gate that does not arm the mechanism is not a gate for it — the same rule that `RingLane`
itself was created to enforce.

### CONFIRMED by measurement — warm-compile admits NOTHING under arena lambdas

RingLane(N=8), same binary, one flag apart:

```
off     LANE_MS=2891     [GILFREE-RINGS] warm-compile 37/46 general-path callees in 2ms
arena   LANE_MS=238098   <no warm-compile line at all>
```

Not a reduced admission rate — the pass reports nothing, i.e. it found no targets to compile where
the flag-off run found 46 and compiled 37. Every ring task then runs its once-per-task callee
INTERPRETED inside the lock, which is exactly the 82x.

So the mechanism is no longer a reading of the source: **`RAVA_ARENA_LAMBDA` poisons the lane's
warm-compile list by keying it on a synthetic arena class that has no bytecode.** The fix named
above — warm-note the lambda's IMPL method via the interned site index instead of the arena class —
is now a fix for a measured cause, and it is the single change standing between the boot's 89%
capturing-lambda population and a mechanism that can serve it.

★ And the general lesson, paid for twice today: `RAVA_ARENA_LAMBDA` looked shipped-ready on ShapeOr
(−14.4% / −11.8%, bit-exact) because ShapeOr is single-threaded and never arms the lane. Four green
correctness gates coexisted with an 82x slowdown, because `LANE_MS` is a correctness yardstick
only. **A gate that does not arm the mechanism is not a gate for it** — the rule `RingLane` was
created to enforce, applied here to the flag rather than to the lane.

### ⛔ The "synthetic class key" fix was BUILT and did NOT help — the pilot never runs

Recording the lambda's IMPL (via `lambda_site_get(arena_lambda_site_idx(p))`) instead of the
synthetic arena class is the right key, and it changed nothing:

```
off     LANE_MS=2727     warm-compile 37/46 general-path callees in 1ms
arena   LANE_MS=224553   <still no warm-compile line>
```

So the earlier inference was wrong, and the disproof is in the code it pointed at. The line is
printed under `if warm_compile_enabled() && pilot > 0`, and the OFF arm prints it with the SAME
binary and the SAME flags bar one. Therefore under `RAVA_ARENA_LAMBDA` **`pilot == 0`**: the lane
never runs a serial pilot task at all. Nothing is recorded because nothing executes to record, and
the key was never the problem.

⇒ The real question moves one level up: **why does the lane admit no pilot when closures are
arena-born?** Admission is the kind-5/6/7 MethodHandle shape test the ring gates were built to
cover; an arena-born closure plausibly presents a different receiver shape to it. That is the next
thing to read — `gilfree_cf`'s admission path against an arena-lambda task — and it is a
DIFFERENT mechanism from the one this section originally named.

The impl-key change is kept: it is inert with the flag off (a non-lambda `Value::Obj` finds no site
and falls through to the old path) and is a strictly better key with it on. But it is kept as
correctness, NOT as a fix — it bought nothing measurable and must not be recorded as having.

★ Two hypotheses, two measurements, one right answer and one wrong one, both from reading the same
file. Naming a mechanism from source is cheap and worth doing; it is not evidence until the
measurement agrees, and this one took a build to refute.

### VERIFIED: the lane does not slow down under arena lambdas — it never ARMS

Grepping the two RingLane runs for the lane's own trace settles it. Flag OFF:

```
[GILFREE-RINGS] pilot#0 SERIAL ms=766 cbs=0 newarray=0
[GILFREE-RINGS] warm-compile 37/46 general-path callees in 1ms
[GILFREE-RINGS] admit#0 RingLane::lambda$main$0 t=769ms
[GILFREE-RINGS] run#0 thread=ThreadId(6) … aborted=false
```

Flag ON: **not one `GILFREE-RINGS` line exists.** No admission, no pilot, no region.

So "a 78x regression on the lane" was the wrong description throughout this section. The lane is
never armed at all, and 224-238s is simply the SERIAL time for a ring that would otherwise run
8-way — which matches the standing note that `LANE_MS` is not a speed yardstick (serial 403857ms
vs armed 2855ms on an earlier tree).

⇒ `RAVA_ARENA_LAMBDA` makes the ring task fail ADMISSION. The admission path tests the task's
MethodHandle shape (kinds 5/6/7) on a lambda receiver that normally arrives as `Value::Ref`; an
arena-born closure arrives as `Value::Obj` and does not match. That is a receiver-shape test that
was written before the third representation existed — the same class of omission as the dozen
`matches!(v, Value::Ref(_))` gates the 2026-07-25 update above had to route through
`Value::is_fn_value()`.

**This is the actionable next step**, and it is small: teach the lane's admission the arena-lambda
shape, exactly as `jit_direct_resolve` was taught it. Until then `RAVA_ARENA_LAMBDA` silently
disables the gil-free lane, which is why it must stay default-OFF regardless of its single-threaded
ShapeOr numbers.

★ Three descriptions of one symptom in one session — "78x lane regression", "warm-compile poisoned
by a synthetic key", and finally "admission refuses the task" — and only the third survived
contact with the trace. The first two were inferred from code that was genuinely relevant and
genuinely wrong. Reading names a candidate; only the log says which candidate is real.

### ROOT CAUSE, at a named line: `plan_meta` accepts only `Value::Ref`

```rust
// gilfree_cf.rs:927  (and the same shape at :535)
fn plan_meta(vm: &mut Vm, task: &Value) -> Option<PlanMeta> {
    …
    let Value::Ref(handle) = task else { return None };
    let captured = vm.heap.lambda_of(handle)?.captured.len();
```

An arena-born closure is `Value::Obj`, so `plan_meta` returns `None` on its FIRST line of real
work. No plan, no admission, no pilot, no region — which is exactly the trace: not one
`GILFREE-RINGS` line exists with the flag on.

This is the same omission the 2026-07-25 update above had to fix a dozen times elsewhere:
`Value::Ref`-shaped gates written before the arena representation existed, which read a valid
closure as "not a function" and silently take a different path. `nio.rs`'s `Files.find` was the
sharpest of those ("listResources abort with no resources — every datapack registry came up
empty"); this one is the same bug wearing a performance costume instead of a correctness one,
because the fallback happens to be *correct* and merely serial.

**The fix** is the one already applied to the consumer surface: accept the arena shape as well,
routing through the arena-lambda accessors (`arena_lambda_site_idx` / `lambda_site_get` for the
identity, the inline `AL_HDR_CELLS` capture cells for `captured`) exactly as `jit_direct_resolve`
does. Both `:535` and `:927` need it, and `resolve_impl` on the next line needs checking for the
same assumption.

⚠ NOT ATTEMPTED HERE. It is a real change to admission — the path that decides what runs off-GIL —
and landing it unmeasured at the end of a session is how a silent divergence gets shipped. The
four ring gates (`ChunkFill 8790f04ace726ecf`, `RingScan`/`RingLane`/`RingLaneInstance`
`37539c6c375d0c3`) plus a RingLane run that must show `admit#0` and a non-zero `warm-compile` line
are the acceptance criteria; the hash alone is not, since every hash in this investigation stayed
green while the lane was silently disabled.
