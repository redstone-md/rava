# The contiguous value stack — the plan, its price, and why it is the only lever left

**⚠ Status, corrected 2026-09-16 against HEAD `479fa2ae` — this plan is partially landed and
the rest is CLOSED.** Stage 1 shipped in commit `578247f8`: bit-exact on all four gates and
**measured NEUTRAL** — compilers-off CPU min 138.9 vs 138.0 s, −0.65%, overlapping against the
±2.5% floor. Stages 2+3 are inseparable (stage 2 alone is a predicted regression) and stage 4
as written is unsound; correction notes are inline at each stage. The remaining slice prices
at **1-2% of the product boot, below the calibrated floor**, so no product implementation is
justified. If it is ever revisited, the honest A/B is source-level two-binary ABBA — a
same-binary kill switch is not representable without a dual-representation alias. Every
number below is measured, not estimated; unless a correction note says otherwise, the numbers
were taken before stage 1 landed — the two-buffer era.

## Why this and nothing else

The standing benchmark is compilers off on both sides. Measured 2026-08-25, one harness
(`bench/jvm_cpu.ps1`), same box, same jar, same world:

| | wall | CPU | cores | `Done` | RSS |
|---|---|---|---|---|---|
| rava `RAVA_JIT=0` | 145.7 s | 141.8 s | 0.97 | **0.612** | 778 MB |
| `java -Xint` | 51.3 s | 59.2 s | 1.15 | 1.952 | 351 MB |

work 2.40× × cores 1.19× = 2.85, against an observed wall ratio of 2.84 — the decomposition
closes to within a percent. **The gap is per-core work.** Parallelism is a 1.19× term here and
no GIL work can move it. (In the compilers-ON configuration the split is the opposite — work
2.56× × cores 3.13× = 8.0× — which is how ledger row 440 got it wrong by generalising from the
wrong configuration.)

We already beat HotSpot on `Done` by 3.2×. The entire deficit is the phase before it.

## The one target that is most of the 2.40×

`bench/FrameCost.java`, both engines, compilers off:

* rava frame intercept **331 ns**, `java -Xint` **17.4 ns** — **19×**
* HotSpot per-opcode slope **1.15 ns**

At ~190M framed calls in a boot, 314 ns of excess each is **≈60 s of the 141.8 CPU-s, 42% of
the whole boot**.

And `bench/FrameBench.java` under the sampler — 42M framed calls and nothing else, the first
profile of the call itself rather than of a boot that drowns it:
*(Pre-stage-1 profile — the two-buffer era whose 7.7%/2.8% rows the stage notes below price
from; stage 1 halved the take/give round trip.)*

```
run_hot              43.2%   (the caller's own loop)
run                  11.8%
invoke_framed        10.5%
invokestatic          6.2%
pool_take + pool_give 7.7%   <-- buffer traffic
invoke_idx            3.3%
arg_layout TLS memo   3.2%
drop_in_place<Frame>  2.8%
pop_args_n            2.0%
Frame::new            1.6%   <-- building the frame itself
```

**The buffer traffic costs five times what constructing the frame does.** `RAVA_POOL_CENSUS`
explains the volume: **84,000,006 takes for 42M calls — two buffers per call, and two gives.**
*(Pre-stage-1 census — stage 1 halved it to one take and one give per call.)*

## What is already ruled out

Do not re-run these:

* **Drop glue on `clear()`** — `pool_take`'s own comment argued for months that clearing a
  `Vec<Value>` runs `Value`'s drop glue per slot and that this is the frame's real cost.
  Measured: **0.50 slots cleared per checkout.** Not the cost.
* **Hoisting the census gates off the pool path** — two `OnceLock<bool>` per take and per give,
  ~336M acquire loads per `FrameBench` run, and the ledger prices that exact mechanism at −15% on
  the `push!` path. Interleaved two-binary A/B: base 312/336/338/401 against hoist
  332/351/355/380. **Overlapping, no win.** The mechanism pays where the volume is per-OPCODE,
  not per-call.
* **Shortcutting the compiled→host callback** past the synthetic frame and the second buffer:
  **+4.25% CPU** even after removing the two `String` allocations that made the first attempt
  +6.91%. A speculative probe in front of a mostly-missing cache costs more than it saves.
* **Outlining cold arms out of `invokevirtual`** — the fatal-path dump came out at −64 bytes of
  a 1752-byte frame. The rest is live code on the taken path, and the file header records that
  splitting the resolved-dispatch tail costs +4.2% to +12.7%.

**The frame cannot be shrunk by moving code out of it. It has to stop being built.**

## The design

One contiguous `Vec<Value>` per thread. A frame's locals and operand stack are a window into it:

```rust
pub struct Frame {
    pc: usize,
    code: *const Code,
    base: usize,      // index of local 0 in the thread's value stack
    nlocals: usize,   // operands begin at base + nlocals
    class: Arc<LoadedClass>,
}
```

* `local(i)` → `stack[base + i]`
* `push(v)` → `stack.push(v)` (the existing hand-written fast path is unchanged in shape)
* `pop()` → underflows at `base + nlocals`, not at 0
* **A call passes its arguments by leaving them where they already are.** The callee's `base`
  is the caller's `sp - argc`; there is no `pop_args_n`, no receiver `insert(0, …)` memmove, no
  second buffer, no take, no give, no drop.
* GC roots: **⚠ corrected — "one range per thread" is UNSOUND.** A grow-only arena leaves dead
  slots with stale bits above a popped frame, and suspended stacks interleave on one
  OS-thread arena (parked green threads, `drive_green` slices, fork worker windows,
  deopt/seam windows), so the region between two suspended stacks is dead while both are
  live — a wholesale `[0, height)` mark would follow a stale `Value::Obj` into a swept,
  reused arena region. Root scanning stays per-frame bounded: each registered frame's
  `[base, base+nlocals+sp)` window, the `frames::all()`/EXEC_REG/green-thread walkers kept in
  their exact shape, reading windows instead of owned `Vec`s. Window pop runs drop glue
  explicitly (truncate-with-drop at the cursor), relocating `pool_give`'s clear — not
  deleting it.

## Staging — each stage compiles, passes the gates, and is measurable on its own

**⚠ Corrected: only stage 1 ever shipped under this discipline — stages 2+3 are inseparable
(one commit or not at all) and stage 4 is unsound as written. See the notes at each stage.**

1. **✅ SHIPPED — commit `578247f8`, bit-exact on all four gates, measured NEUTRAL.** `Frame`
   is one `buf` + `nlocals` — locals `[0, nlocals)`, operands `[nlocals, len)` — with all 121
   sites converted across 24 files. Four-round interleaved two-binary A/B: compilers-off CPU
   min 138.9 vs 138.0 s, **−0.65%, overlapping** against the pre-registered ~2.5%. The table
   below is therefore the migration already in the tree, kept as the historical record. The
   sweep's one real bug — `pop_args_n`'s `set_len`→`truncate` turning a checked-out move into
   a double free — passed all 427 unit tests; the bit-exact gates are the only instrument
   that sees a moved-value bug.

   *The stage as originally written:* **Merge `locals` and `stack` into one `Vec` inside
   `Frame`** (`buf` + `nlocals`). No shared stack yet. Halves the pool traffic and removes
   one `Vec` drop per call. *Expected ~2.5% of the boot — at the instrument floor, so it is
   a step, not a win.*

   **121 sites outside `vmframe.rs`** (72 `.stack`, 49 `.locals`), and they are NOT evenly
   spread — most of the count is in four files that do bulk iteration rather than per-opcode
   work, which is what makes the stage tractable:

   | file | sites | what it does there |
   |---|---|---|
   | `vm/fork_chunk.rs` | 15 | builds worker frames for the fork lane |
   | `trampoline.rs` | 14 | the explicit-stack driver's frame handling |
   | `vm/gc.rs` | 13 | root scan — **becomes ONE loop over the range** |
   | `vm/gc_collect.rs` | 10 | arena root scan — same |
   | `vm/drive.rs` | 8 | return-value hand-off between frames |
   | `vm/fork_counted.rs` | 6 | counted-loop worker frames |
   | `opcodes/invoke_virtual.rs` | 5 | argument pop + the ULE dump |
   | `opcodes/fields.rs` | 4 | get/putfield operand access |
   | rest | 46 | one or two each |

   The three collector files (23 sites) get SIMPLER, not harder: two `Vec` walks per frame
   collapse into one slice walk.

   **⚠ The table counts access/scan sites only — a one-`Frame`-type change also has a builder
   and publish blast radius it omits:** `SeamFrame` marshal in `vm/jit_call_body.rs` (~1017,
   `buf` from `take_vec`, `frames::push` ~1044 — the one conversion that removes work: the
   take/give per crossing, an `nlocals=0` window), `vm/jit_natives.rs:193`,
   `jit/callbacks/deopt.rs:514`, `jit/exception_state/owned.rs:58-63`,
   `vm/fork_iterator.rs:360`, `vm/lifecycle.rs` (`build_frame_idx`, `write_checkpoint`,
   `restore_running` — snapshot slices serialize identically, restore must claim windows),
   `vm/drive.rs` (`recycle_frame` + both `Throw` unwind arms), `vm/fork_chunk.rs:394/554`,
   `vm/fork_counted.rs:154/187`, `opcodes/hot.rs:469`. Arena sizing must also account for
   suspended stacks pinning the high-water mark (green threads, fork workers); the
   `locals().to_vec()` template sites (`vm/fork_chunk.rs:196`, `vm/fork_counted.rs:124`)
   become copies into the worker's own arena window — same copy count as today.

2. **Move the buffer from the pool to a per-thread arena** — `Frame` holds `base`/`nlocals` into
   a thread-local `Vec<Value>` that only ever grows. Removes `pool_take`/`pool_give` (7.7% of the
   call) and `drop_in_place<Frame>` (2.8%).

   **⚠ Corrected: those figures are pre-stage-1, and stage 2 ALONE is a predicted regression —
   stages 2+3 are inseparable.** The 7.7%/2.8% pricing came from the two-buffer-era census;
   stage 1 already halved the round trip to one take+give, and what survives is the args
   buffer `pop_args_n` checks out, which only stage 3's in-place claim deletes. A window
   `Frame` also cannot take ownership of the args `Vec` by move the way `Frame::build` does
   today (`let mut buf = args`), so stage 2 alone ADDS one `n × 24`-byte memcpy per framed
   call and removes nothing measurable. The approvable slice is stages 2+3 in ONE commit,
   all builders at once — a missed builder leaves a `Frame` with a stale `base`, silent
   corruption only the bit-exact gates catch. **On current evidence the slice is CLOSED —
   see the expected-value correction below.**

3. **Pass arguments in place.** `invoke_idx`/`invoke_framed` stop building a `locals` buffer;
   the callee's window starts inside the caller's operands. Removes `pop_args_n` (2.0%), the
   receiver insert, and the `push_call_roots` copy (1.0%).

   **⚠ Corrected: not an independent stage — the inseparable second half of the stage-2
   slice.** `invoke_framed` claims the caller's operand window as the callee's locals (the
   claim reserves callee `nlocals + max_stack` with one grow check);
   `expand_locals_into` stays as the copy arm for wide/unbox descriptors (`arg_layout`
   already memos the case); args drop exactly once at window pop, since the caller's `sp`
   already excluded them at claim — the inverse of stage 1's `set_stack_len` double-free
   lesson. `pop_args_n`/`pop_recv_args_n` are deleted on the framed path only
   (`opcodes/statics.rs` — native/iterate callers keep their `Vec` paths). An index-based
   `base` keeps arena realloc safe as long as no borrow crosses the grow, which the
   raw-pointer `run_hot` already guarantees.

4. **Single GC root range.** `frames::all()` and the three collector paths walk one contiguous
   range per thread instead of two `Vec`s per frame.

   **⚠ Corrected — unsound as written; see the design bullet above.** "One range per thread"
   holds only for pure recursion with zero suspended stacks, which green threads and forks
   violate by construction. What survives is the per-frame window scan; the collector files
   still get simpler (two `Vec` walks per frame collapse into one slice walk), but the range
   stays bounded to each registered frame's `[base, base+nlocals+sp)`.

## Verification, non-negotiable

Every stage: `ChunkFill 8790f04ace726ecf`, `RingScan`/`RingLane`/`RingLaneInstance`
`37539c6c375d0c3`, 427 tests, and all 18 differential probes against `java -Xint`. A stack bug
here corrupts execution silently, and the bit-exact hashes are the only thing that catches it.

Measure with `bench/boot_ab_flag.ps1` (CPU time, ABBA) — **calibrated floor ±2.5%**, verified
against a flag that does not exist (+1.59%, not disjoint). Wall on this box is ±8% and has
produced one false "disjoint" result already (ledger rows 436-437).

**⚠ Stop/go for the closed stages-2+3 slice, recorded for any revisit.** A same-binary kill
switch is NOT representable — an honest env fallback needs both representations resident
with a discriminant branch on every frame accessor, i.e. a dual-representation alias. If
ever revisited: source-level two-binary ABBA with `bench/boot_ab_flag.ps1`; GO only on
(a) all four bit-exact gates plus the 427 tests and all 18 differential probes (stage 1's
double free passed every unit test — only the gates caught it), plus a recursive-depth and
a caught-`StackOverflowError` smoke for the deepest-window truncate-on-unwind path;
(b) compilers-off CPU ABBA disjoint ≥2.5%; (c) product boot not regressed >+1%. Overlap at
the floor is STOP and revert — the stage-1 precedent landed at −0.65% overlapping, and that
is the prior any retry must beat.

## Honest expected value

Stages 2-4 together remove ~13.5% of the call, i.e. **~5.7% of the boot**. That is the largest
single change available and it does not, on its own, reach a boot in seconds — the gap is 2.40×
of work and this is 1.06× of it. It is worth doing because it is the only item above the noise
floor, because it compounds with everything after it, and because every alternative has been
measured and refused. The remaining 2.3× lives in the same place the profile has said for four
sessions: **an engine with two representations of every object, string-keyed caches on hot
paths, and a native ABI that marshals a `Value` buffer at every boundary.**

**⚠ Corrected against what actually happened.** Stage 1 — the step this section priced at
~2.5% — landed NEUTRAL (−0.65%, overlapping). The remaining stages-2+3 slice, with stage 4
cut back to the per-frame window scan, still prices at ~12.5% of the call ≈ 5% of the
compilers-off CPU boot (~6.4-7.5 CPU-s) — above the floor THERE — but at **1-2% of the
product boot, below it** (see the product section below). No product implementation is
justified on today's evidence; the slice is closed unless a new measurement moves its
product share above the floor.


---

# The product configuration's ceiling, measured

**Status: mapped, four ways, with three refuted levers on top. Nothing here is a guess.**

After the lane fix the product boot is **48.2 s wall / 50.9 CPU-s / 1.06 cores / `Done` 0.501**
against `java`'s **5.6 / 17.9 / 3.20 / 0.187**.

## The two configurations, re-measured today

| | wall | CPU | cores | `Done` | RSS |
|---|---|---|---|---|---|
| rava `RAVA_JIT=0` | 122.2 s | 120.1 s | 0.98 | **0.515** | 794 MB |
| `java -Xint` | 45.2 s | 52.5 s | 1.16 | 1.753 | 354 MB |
| rava, product | ~47.5 s | ~50.5 s | 1.06 | **0.49** | ~1250 MB |
| `java` | 5.8 s | 18.7 s | 3.24 | 0.214 | 562 MB |

⭐**Read down the wall column: our compiler multiplies the boot by 2.6× (122.2 → 47.5) and
HotSpot's by 7.8× (45.2 → 5.8).** The interpreter gap is 2.70×; the compiled gap is 8.2×. So the
JIT is where the distance is being LOST, not the interpreter — and the measured reason is in this
document: only **8.5% of block A's time is inside compiled code**, because compiled code leaves
for the host on 36% of its dynamic calls and for a native `java.util` on top of that.

⛔**AND A CORRECTION THAT WAS REPEATED ALL SESSION.** `Done` was reported as "2.3-2.4× faster
than HotSpot". That compares rava's PRODUCT `Done` against `java -Xint`'s, which is not the same
engine. Read the column properly:

* against `java -Xint` (0.515 vs 1.753) rava's `Done` is **3.4× FASTER** — true, and that is the
  standing benchmark's comparison;
* against `java` (0.42 vs 0.187-0.214) rava's `Done` is **about 2× SLOWER**.

So the goal's `Done` clause is met in the compilers-off comparison and **not** in the product one.

## Where the 48 s is

| block | rava | java |
|---|---|---|
| start → `Environment:` | **32 s** | ~4 s |
| datapack reload | **16 s** | ~1 s |
| rest | 1 s | <1 s |

## What both blocks are made of

RIP sampling, i.e. where the instruction pointer actually is:

* block A — **8.5% inside compiled code, 91.5% in the Rust host**
* reload — **8.1% / 91.9%**

Category sampler on block A: **NATIVE_BRIDGES 27.7%**, JIT_CALLBACK 22.5%, INTERP_EXEC 9.3%,
VM_DISPATCH 3.4%, CLASS_LOADING 0.4%. (The same sampler says JIT_EXEC 36.7% — **do not believe
it**; RIP on the same window says 8.5%, and the ledger already records every JIT_EXEC figure as
inflated.)

The function profile is flat in both: block A's top real host row is `jit_call_body` at 2.2% of
live samples, the reload's is `invokevirtual` at 2.5%.

Block A's bridges, however, are **not** flat — four call sites hold **68%** of them, all in the
block-state machinery: `StateHolder@cp199 → Map.get` 26.0%, `Reference2ObjectArrayMap@cp64 →
reference2ObjectEntrySet` 18.8%, `Shapes@cp299 → Objects.equals` 16.3%,
`Reference2ObjectArrayMap@cp68 → ObjectSet.iterator` 7.0%.

## The ceiling, priced — `bench/NativeCost.java`

| | rava | java |
|---|---|---|
| `Map.get` | 820 ns | 2 ns |
| `Objects.equals` | 687 ns | ~0 ns |
| `Map.get` (`Integer` key, alloc-free `MapKey::Int`) | 55 ns | 1 ns |
| control loop | 13 ns | ~0 ns |

**Our `java.util` replacements cost 200-600× HotSpot per call.** That is what NATIVE_BRIDGES'
27.7% is: not routing (the whole cascade is ~3% of block A) but the natives' own work. HotSpot
has no bridges at all — its `java.util` is compiled Java that inlines into the caller, and ours
is a native that cannot.

⚠**Two traps in this benchmark, both of which produced a wrong number first.** Put every loop in
`main` and nothing tiers up (no OSR), so the control loop reads 44 ns/iter and every figure is an
interpreted figure wearing a product label. And `containsKey` is declared PURE in
`native_contract.rs`, so on repeated keys it is served from a MEMO — its 42 ns is not the price of
a native call and is useless as a baseline.

## Three refuted levers — do not re-derive them

1. **Identity map keys by class contract.** Sound from the JDK contract, and it killed the boot
   twice (`Errors during registry creation`). Reason in `map/keys/mod.rs`: the engine has two
   representations of every object, so a structural key matched across a `Value::Obj`/`Value::Ref`
   seam that an identity key cannot.
2. **Cached field ids on the hot map path.** `field_id_existing` is 18.7% of the main thread on a
   pure `Map.get` loop. Converting the by-name probes moved the microbench 864→820 ns and the boot
   not at all (ABBA 48.9/49.1 on against 48.6/48.9 off). The boot's block-state maps take the
   r2omap identity scan and never enter that prologue — price a mechanism on the stream it will
   actually run.
3. **Alloc-free identity keys on maps that were already identity-keyed.** Correct, bit-exact,
   shipped — and **measured neutral on the boot**, because the same r2omap path bypasses it.

## JIT_CALLBACK is the same boundary, counted twice

`[PC-CB]` (new) prints the callback population with every profile block. Block A:

```
total=62,210,890 | invoke=55,195,044 (88.7%) newarray=3,407,862 (5.5%) new=1,626,801 (2.6%)
instanceof=1,154,774 (1.9%) aastore=286,879 (0.5%) putfield=133,717 (0.2%)
```

**88.7% of every excursion out of compiled code is `invoke`.** The inliner is not the reason:
it exists, its depth is already 8 under Tier-0, and the depth was swept in both directions and
found worth about a percent either way. These 55 M calls are the ones that CANNOT be inlined —
because what they call is written in Rust.

⚠**An earlier version of this section claimed JIT_CALLBACK was the same java.util work
NATIVE_BRIDGES counts, reached from compiled code — that is WRONG and the contract in
`vm/jit_call.rs` says so in as many words:**

> The callee's own tier (interp/jit) and native bridges push their own category and override this
> for their duration, so **JIT_CALLBACK ends up as the pure miss-to-dispatch tax**.

So the two categories are disjoint and they ADD. NATIVE_BRIDGES 27.7% is the natives' work;
JIT_CALLBACK 22.5% is **pure dispatch overhead** — 22.5% of 32 s over 55.2 M `invoke` callbacks is
**~130 ns per dispatch**, against roughly 1 ns for a monomorphic compiled call on HotSpot.

### And the dispatch-table zero has a mechanism, not a coincidence

`[WALL-SITE]` attributes JIT_CALLBACK per call site, and block A's are not java.util at all —
they are **interface and lambda calls into guest code**:

| share | site |
|---|---|
| 8.6% | `IndirectMerger@cp48 → IndexMerger$IndexConsumer.merge` |
| 5.9% | `DiscreteCubeMerger@cp41 → IndexMerger…` |
| 5.6% | `StateHolder@cp199 → java/util/Map.get` |
| 5.3% + 3.8% + 1.8% | `…→ Direction$Axis.choose(III)I` (an enum virtual, three receivers) |
| 5.0% + 4.4% | `Reference2ObjectArrayMap → Iterator.hasNext/next` |
| 3.6% + 3.3% | `…→ BooleanOp.apply(ZZ)Z` (a functional interface) |

⭐**A machine-code PIC HIT still leaves machine code.** The exit to `jit_call` happens either way;
the PIC only saves the re-resolution behind it. That is why turning 35 M misses into hits raised
the rate 39.3 → 45.9% and bought nothing, and it closes the "better PIC" line **by mechanism
rather than by observation**. The cure is not a better cache, it is not taking the excursion:
a compiled→compiled direct call is **0.6 ns, level with the JVM** (ledger, `NO-OSR is the gap`).

Why these sites cannot be compiled→compiled is already written down in
`docs/lambda-machine-code-pic-blocker.md`: a machine-code hit arm must materialise the callee's
captures into the call buffer, and they live in a heap `Vec<Value>` that Cranelift cannot decode —
so it is **downstream of the arena-lambda representation**, which is default-OFF and crashes a
real boot. The same root as the identity-key refutation above: one representation per object.

## The arithmetic, so the next attempt starts calibrated

Block A makes **8.26 M native calls** (the `[PC-ARM]` hit+miss tally) and spends **27.7% of 32 s
= 8.9 s** in NATIVE_BRIDGES ⇒ **~1.08 µs per native call**, which matches the microbench's 820 ns
independently. HotSpot does the same calls at 2-5 ns.

So making our `java.util` free takes block A from 32 s to about **23 s**, and the boot from 48 to
about 39. Removing the dispatch tax as well would take block A to about **16 s** and the boot to
about **32**. **Both ceilings are real, they add, and together they are still not the whole gap.** Nothing in this engine is one
change away from the target; the other 23 s is compiled MC code, the interpreter, and the
callback path, in the flat distribution four profiles now agree on.

Note for anyone reaching for the obvious cure: rava loads **no JDK bytecode at all** — no jmod,
no rt.jar, no java.base. Every `java.*` class is a Rust native. That is what makes this boundary
un-inlinable in principle rather than by omission, and it is the decision the next architectural
step has to either keep and work around (emit the hottest operations as IR inside the caller) or
change.

## The sharpest probe available, and it is 54x

`bench/ShapeOr.java` reproduces `WallBlock.makeShapes` — the boot's largest NAMED concentration
inside `Bootstrap.bootStrap()`. It runs in 14 s, exits on its own, and is bit-exact against the
JVM (`SUM=3cfa327f01e0c901`):

| | HOTNS |
|---|---|
| rava | **13.908 s** |
| `java` | **0.256 s** |

**54×**, against 8.6× for the boot as a whole. Anything that fixes block A must move this, and it
takes 14 s to ask instead of 50.

Its profile is flat like every other — top host row `jit_direct_invoke` at 3.6% of the main
thread — but the rows have a SHAPE the boot's do not: `jit_direct_invoke`, `jit_call_body`,
`jit_indy`, `invokedynamic_assemble`, `jit_direct_probe`, `fu_double_view`, `jit_fu_invoke`,
`rava_dispatch_miss` together are **19% of the main thread and all one mechanism — calling a
lambda from compiled code**.

Two flags were tried on it and both are bit-exact and neutral (13.908 / 13.953 / 14.047):
`RAVA_ARENA_LAMBDA` and `RAVA_LAMBDA_PIC`. The first matters anyway, because
`docs/lambda-machine-code-pic-blocker.md` recorded that arena-lambda **crashes a real boot**, and
that is now stale — it boots clean (`wall 49.6 s, Done 0.500`) since the arena guard fix. So the
λ-PIC is no longer blocked by a crash, and its `ADMITTED = 0` census needs a different
explanation than the representation.

### ShapeOr, decomposed by dispatch (`RAVA_JIT_PIC_PROBE`)

```
dynamic calls out of compiled code = 60,334,711
  machine-code PIC hit (no Rust)     38,461,746 (63.7%)   <- never leaves machine code
  fell to Rust host                  21,872,965 (36.2%)
      PIC miss -> rava_dispatch_miss 19,952,547 (33.0%)
        lambda-receiver misses 0  directable 0
        capturing arena-λ calls: self_reg = 7,666,163
        capture cells: prim 32.9M  arenaptr 31.5M  array 13.5M  ref 0.95M
      plain invoke -> rava_invoke     1,920,418 (3.1%)
```

Three facts fall out of that, and the third is the next step:

1. **A machine-code PIC hit does not leave machine code** — 63.7% of the calls are already free.
   Raising coverage is therefore a real lever here, unlike the dispatch-table result, which was a
   different mechanism.
2. **`directable = 0`.** The λ-PIC has nothing to serve because **every** lambda on this workload
   CAPTURES, and it only serves non-capturing ones. Its `−0.18%` null is a mechanism with an
   empty population, exactly as `pic_fill.rs` already suspected.
3. **7.67 M capturing arena-λ calls go through `self_reg`**, materialising ~79 M capture cells.
   These are already ARENA objects by default — the representation the blocker doc says a
   machine-code hit arm needs. Serving them in machine code converts **12.7% of all dynamic calls
   on this workload** from a host excursion into a direct call, and the crash that blocked
   building it is fixed.

### The reload window's bridges, and two more nulls

The datapack reload is 16 s against `java`'s ~1 s — a **16×** ratio where the boot as a whole is
8.6× — so its bridge table is where a second `Objects.equals`-shaped concentration would show.
It has one:

| share of NATIVE_BRIDGES | bridge |
|---|---|
| ~27% | `java/util/stream/Stream#map` (across its sites) |
| 15.6% | `java/util/Map#get` |
| 11.8% | `Reference2ObjectMap#reference2ObjectEntrySet` |
| ~8% | `Stream#collect` |
| 4.5% | `ObjectSet#iterator` |

`Stream#map` applies a guest lambda per element through `vm.apply`, which is the same
host↔guest-per-element shape the whole ceiling is made of. **`RAVA_LAZY_STREAMS` — the fused
lazy-stream implementation already in the tree — was re-measured on today's engine and is
+1% WORSE** (48.1/47.8 against 47.0/47.5), confirming the ledger's earlier null rather than
inheriting it.

And on the probe, **the inline depth does not matter**: 8 → 10.622/10.668 s, 16 → 10.695/10.699,
24 → 10.760, monotone but flat. Deeper inlining cannot reach the lambda call sites, because the
inliner has no devirtualised target for an interface receiver — which is the same wall the λ-PIC
hits from the other side.

## ⚠ THE PLAN'S HEADLINE ITEM IS PRICED FOR THE WRONG CONFIGURATION

This document opens by pricing the contiguous value stack against the COMPILERS-OFF benchmark,
where the frame is 42% of the boot. That is still true there. It is **not** the product boot's
shape, and the product boot is the one that is 47.5 s:

| category | product block A |
|---|---|
| JIT_CALLBACK (compiled → host dispatch) | **22.5%** |
| NATIVE_BRIDGES | 20-27% |
| VM_DISPATCH (the interpreted invoke) | **3.4%** |
| INTERP_EXEC | **9.3%** |

The contiguous stack acts on `VM_DISPATCH + INTERP_EXEC` — **12.7% here, not 42%** — so its
~5.7% of the compilers-off boot is worth on the order of **1-2%** of the product one. The
compiled-side dispatch is five times larger.

**⚠ Closed on this evidence (2026-09-16):** stage 1 of the contiguous stack has since landed
(`578247f8`) — bit-exact, measured neutral — and the remaining stages-2+3 slice, worth this
1-2% of the product boot, is BELOW the calibrated ±2.5% floor. It is not justified as a
product lever; the compilers-off configuration where the frame is 42% is the only one where
it pays, and no product decision may be made on that number. Reopen only on new
product-side evidence.

Three independent results this session point at the same place:

* **790,000 interpreted method entries became compiled ones and the time did not move.** So the
  cost of a call is not the callee's tier — it is the invoke.
* **Inline depth 8 → 16 → 24 is flat on the probe.** The inliner cannot reach the lambda sites,
  because an interface receiver gives it no devirtualised target.
* **`directable = 0`** — every lambda on the probe CAPTURES, and the λ-PIC only serves
  non-capturing ones, so it has an empty population by construction.

⇒ **The next engine step for the PRODUCT configuration is the machine-code arm for capturing
arena-lambda receivers**, not the contiguous stack. Sizing, and it is SMALLER than a first pass suggests.
7.67 M of the probe's ~20 M host excursions are capturing-λ calls, i.e. 38% of them. Taking
JIT_CALLBACK's 28.7% as the base gives ~11% of the probe — but that base is the SAMPLER's
category, and on this same window the sampler says JIT_EXEC 52.3% where RIP sampling says 11.3%,
so it is not trustworthy as a denominator. The RIP view of the same machinery —
`jit_direct_invoke` + `jit_call_body` + `jit_indy` + `invokedynamic_assemble` + `jit_direct_probe`
+ `jit_fu_invoke` + `rava_dispatch_miss` — is **13% of the main thread**, and 38% of that is
**≈5% of the probe**, i.e. roughly **2-3% of the boot**.

**Where its buffer comes from, which is the non-obvious part.** The impl expects its locals as
`[cap_0…cap_{n-1}, arg_0…]`, while the hit arm hands over `[recv, arg_0…]` — so the thunk needs a
buffer of its own. Emit a Cranelift **stack slot of `buf_len * 8`** (both known when the thunk is
compiled), fill `[0, cap_n)` from the receiver's cells, copy the incoming args into `[cap_n, …)`,
and call the impl with the slot. **Rooting comes free from the callee**: the impl is `self_reg`,
and its Deep-IC v3 prologue pushes `(buffer, frame_len)` onto the thread's cell root stack — the
buffer being exactly the pointer the thunk passes. A collection inside the impl therefore sees the
captures as roots without the thunk emitting any rooting of its own.

**The emit is straight-line, because the capture KINDS are static per site.** `arena_lambda_cell`
returns `(raw_cell, kind)` and reads the kind from the object, but the kinds are a property of the
capture SHAPE (`nextgen::lambdas` builds one class per shape, `shape.cap_kinds`), so at
thunk-compile time every kind is a constant. Bake them; then per slot the emit is:

* **prim** (`kind < 4`) — identity copy. `pack_local`'s width follows from the baked kind, so no
  test is emitted.
* **reference** (per `cap_ref_mask`) — one null test, then a 2-bit switch on `raw & 3`:
  `1 → (raw>>2) | REF_TAG`, `2 → (raw>>2) | ARRAY_TAG`, `0 → raw` (an arena pointer, identity).
  `3` is the `arena_box` index that would need `box_ref` — **and `[PC-CAP]` measures it at ZERO on
  both the boot and the probe**, so the thunk may simply refuse to be built when a shape declares
  one, and the site keeps today's Rust path.

✅**VERIFIED FROM BOTH ENDS.** `jit::abi::box_ref` confirms the same shapes independently:
`Ref(h) → h | REF_TAG`, `Array(h) → h | ARRAY_TAG`, `Null → 0`, `Obj(p) → p.addr()` (identity),
and everything else → `box_push`, the un-emittable case. Composed with `decode_arena_cell` the net
per-slot transform is exactly the shift/or pair above, so the thunk emits no host call.

⚠**AND THE `raw & 3 == 0` ARM IS ONLY IDENTITY WHILE PINNED MIRRORS ARE OFF** — the same
precondition appears on BOTH sides, which is what makes it the design's single global guard:
`decode_arena_cell` probes `pinned_handle_of` on that arm, and `box_ref` probes
`pinned_mirror_get` under `pinned_ptrs_enabled()`. The census counts
24.1 M `arenaptr` cells as "identity", but it classifies them on `cell & 3` alone — and
`decode_arena_cell` follows that arm with a `pinned_handle_of(addr)` probe and returns `Ref(h)`
when the address is a pinned mirror. Today the pinned region is unmapped so the probe is always
`None` and identity holds; a thunk that bakes identity would go silently wrong the day pinned
mirrors flow. Either emit the probe, or make the thunk refuse to be built while
`nextgen::pinned_region_mapped()` — whichever is cheaper — but do not inherit "identity" from the
census without re-reading `decode_arena_cell`.

The signature is the one the JIT already uses for a body: `(locals_ptr: i64, ctx: i64) -> i64`,
i.e. `extern "C" fn(*mut i64, i64) -> i64`, so the thunk and the impl share it and the hit arm
needs no new ABI. `finish_compile(module, ctx, sig, class_ptr, descriptor, ret, self_reg)` is the
existing declare/define/finalize helper and takes an ad-hoc `Context`; `class_ptr` is dereferenced
only under `RAVA_JIT_DUMP`.

⛔**Do NOT compile a second full body with an unpacking prologue instead.** `emit_deep_prologue`
is parameterised and that would be the easy route, but a boot COMPILES ONCE AND MOVES ON — this
tree's standing code-size lesson is that body-duplicating optimisations lose however good the
microbenchmark. A thunk is `cap_n` shift/or pairs plus a call; a second body is the whole method.

⚠**Do not start this expecting 11%.** Prefer the RIP number over the category number whenever the
two disagree, which on this engine they do by 4-5×. The Rust direct-call path already carries the larger share (turning
it off costs +58%); what is left to win is the host round trip on top of it.

`directmono` already proves one half is buildable: it emits NON-LEAF machine-code direct calls
(`fptr|1`, spill → call → reload, callee roots its own frame) for the static/special pool. And a
SITE-keyed lambda dispatch key already exists and is already emitted —
`nextgen::lambdas::lambda_table_key` = `LAMBDA_KEY_BASE + site_idx`, read from the `AL_SITE_SLOT`
cell, with `dispatch_table_probe` as its machine-code twin. Identity can never hit for a capturing
lambda, so a site key looks like exactly the missing piece.

⛔**IT IS NOT. `RAVA_LAMBDA_SITE_CID=1` IS 3.3× SLOWER ON THE PROBE** — 35.326/35.994 s against
10.509/11.031, bit-exact throughout. One key per lambda SITE multiplies the dispatch-table key
space by the number of sites (the table's own budget note sizes a boot at ~29 MB across 920
CLASS ids), and the tables and caches behind it thrash. So the existing site-key mechanism cannot
be reused as-is, and the ledger's "dispatch table refuted at zero" was measuring the mild end of
this.

### One cause, three symptoms — and the code counts it

⚠**AND THE REFUSAL COUNTERS DO NOT SUPPORT THIS, WHICH WAS PUBLISHED BEFORE THEY WERE READ.**
`[PC-DTR]` (new) prints the split, and on BOTH the probe and a real boot every clause reads ZERO
with `ADMITTED=0` — so `note_serve_direct` is not refusing the population, it is **never reached**
(its first guard returns silently when `cache_ptr == 0`, with no counter). Worse, the census is
not self-consistent: the same boot reports `serves=0` alongside `fills=3233`, and `store` is only
reachable through the two `note_serve*` paths that increment `serves`. **Do not build a plan on
these counters until that contradiction is resolved.**

What the code's own comment records — "lambda machinery refused 6,629,289 of 6,629,289 and
ADMITTED 1,442" — is a historical boot measurement, and the clauses it names are still the right
description of the DESIGN:

* `learn_refused(7)` — the SITE key is off. It is off by default, and turning it on is the 3.3×
  regression measured above.
* `learn_refused(8)` — `if entry.cap_n != 0`, i.e. **every capturing lambda**, because the emitted
  hit arm cannot prepend captures.

What IS measured today, on a real boot with the probe armed:

```
mc_probes=34,050,747  mc_hits=1,014,208  (3.0% hit)  promoted_sites=700
table_entries=2,867,200 (~22.4 MB)   miss_shape[ arena=33.1 M  heapref=17.8 M ]
```

**A 3.0% hit rate.** And an A/B of the whole mechanism on the boot is wall-neutral —
47.1/47.1 s with it against 47.3/47.3 without — but its RSS is **1366/1386 MB against
1110/1075 MB**. So the dispatch table is not a wall lever in either direction; what it actually
costs is memory.

⇒ What is actually missing is a **per-site capture-unpacking thunk**: a small compiled body whose
signature matches the call buffer the PIC hit arm can already produce (`[recv, sam_args…]`), which
loads `cap_n` captures from the receiver's arena cells per `cap_ref_mask` and jumps to the impl.
The PIC entry then points at the THUNK, so the hit arm needs no capture logic — that is
`learn_refused(8)` gone.

✅**AND THE FEASIBILITY QUESTION THE CODE ASKED IS NOW ANSWERED, ON THE BOOT.**
`jit_direct_invoke`'s capture loop carries a census that classifies each cell exactly as
`decode_arena_cell` discriminates it, precisely because one of the six kinds — an `arena_box`
index needing `box_ref` — **cannot be emitted**. It was readable only at process exit, i.e. never
on a boot; `[PC-CAP]` now prints it from the periodic dump. A full product boot reads:

```
prim=24,471,803  null=0  ref=243,936  array=4,511,410  arenaptr=24,060,439  BOXIDX=0
```

**53.3 M capture cells and ZERO of the un-emittable kind**, on the boot and on the probe alike. So
every cell a thunk would have to copy is one of four shapes, and all four are one or two
instructions: `prim` and `arenaptr` are identity copies, `ref` is `(cell>>2) | REF_TAG`, `array`
is `(cell>>2) | ARRAY_TAG`. No host call is needed anywhere in the copy.

## ⛔THE CAPTURE PREPEND WAS NEVER THE ONLY BLOCKER — THE KEY IS, AND BOTH ROUTES ARE MEASURED DISASTERS

The thunk above is built and unit-verified, and it removes `note_serve_direct`'s `cap_n != 0`
refusal. It still cannot be served, and the reason is the OTHER refusal — the site key — which
this document previously waved through as "one extra load, free in a PIC". That is wrong, and the
tree already measured both ways of doing it:

* **In the dispatch table** — `RAVA_LAMBDA_SITE_CID=1` is **3.3× slower** on `ShapeOr`
  (35.3/36.0 s against 10.5/11.0). One key per lambda site multiplies the table's class-id key
  space by the number of sites.
* **In the 4-way PIC** — `dispatch_pic.rs` states it directly: computing the lambda key before the
  probe "put a bitmap load, a receiver load and a select on EVERY dispatch", measured at
  **ChunkFill 4,589 ms → 60,282 ms**. A **13× regression.** That is why the key computation was
  moved inside `emit_table_probe`, i.e. onto the MISS path — where it is cheap and useless for a
  hit.

A capturing lambda therefore needs a key the PIC can compare **without** loading anything extra
per dispatch, and neither the class id (shared by every site of one capture shape → wrong impl)
nor the site cell (costs 13× where it must be read) is that. **This is the actual wall, and it is
a representation question, not an emit one**: the same root as the identity-key refutation — the
receiver does not carry a cheap identity the emitted code can key on.

### The way through: let the THUNK check the site, not the key

Both measured disasters share one assumption — that the DISPATCH KEY must distinguish sites. It
does not. Give the thunk the site guard instead:

* the PIC keeps keying on the plain `cid` (the capture shape) — **zero extra instructions on the
  hit path**, which is the whole reason both other routes failed;
* the thunk becomes per **(call site, impl, shape)** rather than per (impl, shape), and opens with
  one load of `AL_SITE_SLOT` and one compare against a BAKED site index;
* on match it unpacks the captures and calls the impl, exactly as it does today;
* on mismatch it calls `rava_dispatch_miss` with the call site's own `cache_ptr`, `caller_ptr`,
  `cp` and `kind` **baked in as constants** — all of which are known when the thunk is built,
  precisely because the thunk is per call site.

So the site check costs one load and one compare, paid only AFTER the PIC has already hit, on a
path that today leaves machine code entirely. Nothing is added to the dispatch of a non-lambda
receiver, and the class-id key space is untouched — the two things that killed
`RAVA_LAMBDA_SITE_CID` (3.3×) and the pre-probe key computation (13×).

### Where the install has to happen — the last plumbing question

The thunk is built, guarded and gate-verified, and wiring it into `dispatch_miss_resolve` proved
the hook cannot live there: `[PC-THUNK]` reads **`offered=13 refused=5 installed=0`** against
7.67 M capturing-lambda calls, because the negative cache runs that resolve ONCE per (site, class)
— the first miss writes `-1` and it never re-runs — and that single shot lands before the lambda's
impl is compiled, so `DirectEntry::fptr` is not yet a pointer.

The install needs two things that currently live in different halves of the miss:

* `cache_ptr`, the site's 4-way PIC, which `rava_dispatch_miss` has;
* a resolved `DirectEntry` whose `fptr` is a real compiled pointer, which `jit_direct_call`'s
  `DirectProbe::Hit` arm has, on every call.

⚠And `learn` is NOT that bridge: it is a take-once breadcrumb gated by `RAVA_MAIN_DT_LEARN`
(default OFF), and its take-once discipline is a SOUNDNESS argument — publishing
`(outer site, inner receiver class) -> inner fptr` from a nested call would make the outer site
call another class's method. ⛔The flag itself was re-measured on today's engine and is **not a
win**: 48.6/47.6 s against 46.8/47.5 with `Done` flat, so the main-thread publication that row 330
worth −20.8% of `Done` for WORKERS does not reproduce here.

So the remaining step is to carry a resolved-and-compiled entry back to the point that holds
`cache_ptr`, under the same take-once discipline. That is plumbing, not design.

### The wiring, located — TWO changes, not one

Every piece is now identified, so the remaining work does not need another search:

* **The emitter** is `jit/emit/cap_thunk.rs`, built and unit-verified (3 tests), gated by
  `RAVA_CAP_THUNK`. It needs the site guard added: one `AL_SITE_SLOT` load, one compare against a
  baked site index, and a `rava_dispatch_miss` call with `cache_ptr`/`caller_ptr`/`cp`/`kind`
  baked — all known because the thunk becomes per call site.
* **The fill point is `vm/dispatch.rs` ~104-115**, where `(class_id, fptr)` is written into the
  4-way arena PIC: when the receiver is a capturing arena lambda and a thunk builds, store the
  THUNK's pointer under the plain `class_id`.
* ⚠**AND A THIRD REFUSAL SITS IN FRONT OF IT, which an earlier draft of this section missed.** The
  `never` chain in the same file reads `else if cls.starts_with('$') { Some(1) }` — and every
  lambda class is `$Lambda`, so a lambda receiver is declared PERMANENTLY non-cacheable before the
  fill is ever reached. Worse, the comment above it says the verdict is permanent: `dispatch_miss`
  writes a `-1` negative entry and the resolve never re-runs for that pair. So the wiring is two
  changes, not one, and the second is a `never`-clause exemption for a lambda receiver that has a
  thunk.
* **Nothing in `dispatch_pic.rs` changes.** The arena hit arm already calls
  `fptr(bufaddr, array_desc_cell)` and the thunk's signature is exactly that; and the `shift_arm`
  is unnecessary because the thunk CONSUMES `raw[0]` rather than dropping it.
* `note_serve_direct`'s `learn_refused(8)` (`cap_n != 0`) can then be lifted for entries that have
  a thunk.

⚠It also means more thunks — one per (site, impl, shape) instead of per (impl, shape) — but they
are `cap_n` shift/or pairs plus a compare and a call, so the code-size lesson that rules out a
second full body does not reach them.

Row 333's class-per-site is a different thing and stays refuted: it multiplied the class space,
and a class is consulted per object and per birth (8×). This adds nothing to any object.

⚠**And if it is ever solved, it must be the 4-way PIC, not the dispatch table.** The table indexes by CLASS ID, and one
capture-shape class is shared by many sites, so a class-keyed entry would call another site's
thunk — which is exactly why `learn_refused(7)` demands the site key, and why the site key costs
3.3× (it multiplies the table's key space by the number of sites). A per-site 4-way PIC has no
such space: its key is whatever the emitted select computes, so one extra load of `AL_SITE_SLOT`
makes it site-keyed for free. `DirectEntry` already
carries `cap_n`, `cap_ref_mask` and `site_key`; the arena layout already puts the captures at a
fixed offset. Nothing here needs the representation rewrite, and nothing here widens a key space —
which is the specific way the obvious design fails.

### The last unnamed population: 91.4% of the misses are ARENA receivers

`[PC-DT]` is new — the dispatch-table census now prints from `pc_profile`'s periodic dump, because
`report()` used to print only from the lane probe's reporter and a microbenchmark has no lane, so
the one number that names the misses was unreadable on the sharpest probe in the tree. With
`RAVA_JIT_PIC_PROBE` armed it reads:

```
mc_probes=15,631,993  mc_hits=0  serves=0  fills=9  promoted_sites=27  (~864 KB of tables)
miss_shape[ null=0  arena=15,639,032  heapref=1,471,060  boxed/other=0 ]
```

**91.4% of PIC misses are arena receivers.** That settles where a machine-code arm has to work —
on `Value::Obj`, not on heap handles — and it explains `[HEAPPIC-LIVE] cache fills=0` on the same
run: the heap-Ref PIC is irrelevant to this workload by population, not by policy.

⛔And the dispatch table **probes 15.6 M times, hits ZERO, and costs nothing measurable**:
10.581/10.653 s with it against 10.730/10.639 without, overlapping. It is inert here rather than
harmful — the emitted probe is evidently a couple of inline instructions — so `RAVA_NO_DISPATCH_TABLE`
is not a lever, and the 864 KB it reserves is the only real cost.

### The `Done` window is a SPIN, and it is flat too

`Done` is the one clause of the goal within reach — 0.42 s against `java`'s 0.187, a 2.2× gap
rather than 8×. Profiled at a 200 µs sampling period (the default 10 ms yields 15 samples over the
whole window and cannot attribute anything), its guest methods are:

```
0.9% MinecraftServer.pollTaskInternal      0.9% DistanceManager.runAllUpdates
0.7% BlockableEventLoop.pollTask           0.6% ThreadedLevelLightEngine.tryScheduleUpdate
0.4% ServerChunkCache.runDistanceManagerUpdates   0.3% Util.getNanos
```

**All polling.** The window is `MinecraftServer.managedBlock` spinning until the spawn chunks are
ready, and the spawn area's own work is 11 ms. So the 0.42 s is per-iteration drain overhead, not
the tasks — and, like every other window measured this session, nothing in it is above 1%.

That completes the characterisation: **block A, the datapack reload, the `Done` window,
`bench/ShapeOr.java` and two microbenchmarks — five independent profiles, all ~91% host Rust, all
flat.** The engine is uniformly 2-8× off at every boundary. There is no concentration left to find;
there is a shape to change.

### The opt-in flag sweep, re-run on today's engine

A lever's value is not a constant of the lever, so the plausible opt-ins were re-measured on the
product boot after it went 112.7 → 47.3 s. Baseline 47.4 s / `Done` 0.415:

| flag | wall | `Done` | verdict |
|---|---|---|---|
| `RAVA_AOT_CLIF=1` | **132.1 s** | 0.593 | ⛔**2.8× WORSE** |
| `RAVA_MAPKEY_SALT=1` | 47.5 s | 0.573 | ⛔`Done` +38% |
| `RAVA_FORK_SHAPES=1` | 47.3 s | 0.418 | neutral |
| `RAVA_JIT_PARALLEL=1` | 46.8/47.0 | 0.405/0.405 | wall neutral, `Done` −2.2% (disjoint) |

`RAVA_JIT_PARALLEL` is the only one that moves anything, and it is **8 ms of `Done`** in exchange
for running guest loops on real threads — a correctness surface out of all proportion to the
prize, and the ledger already records "MC not winnable this way". Not shipped.

## What is left, and it is not a micro-lever

Four independent measurements now say the same thing: both phases are ~91% host, the function
profile is flat, and no site-level change has cleared the noise floor. The gap is the engine's
shape, and the two named items are the ones this document opened with — **one contiguous value
stack**, and **a native ABI that does not marshal a `Value` buffer at every boundary** — plus the
one those refutations added: **a single representation per object**, without which identity-based
fast paths are unsafe anywhere near a map.

**⚠ Update: the "one contiguous value stack" item above is CLOSED** — stage 1 landed neutral
(`578247f8`) and the remaining slice prices at 1-2% of this boot, below the floor. The live
items here are the native ABI and the single-representation work — the dispatch-boundary
step this document already names as the next product move.

---

# Appendix: the other engine change — SOLVED, and not where this appendix said

**Status: fixed and shipped. The design proposed below it is retired; the record of why is kept
because two of its three conclusions were wrong and the third was wrong for the right reason.**

## What the product default boot actually did

| | wall | CPU | cores | `Done` | RSS |
|---|---|---|---|---|---|
| rava, blanket drain (before) | 112.7 s | 290.5 s | 2.57 | 64.303 | 1375 MB |
| **rava, scoped drain (now)** | **48.2 s** | **50.9 s** | 1.06 | **0.501** | 1262 MB |
| `java` | 7.2 s | 23.5 s | 3.25 | 0.202 | — |

**`Done` is now 2.4× faster than HotSpot's**, and the boot lost 64.5 s of wall and 240 CPU-s.

## What it really was

Not makespan, not the nursery, not the GIL-held park. `future_method` drained the WHOLE deferred
batch at **every** `CompletableFuture` method on **any** future:

```rust
if !matches!(name, "supplyAsync" | "runAsync") && has_deferred(vm) { drain(vm); }
```

`GILFREE-RINGS` had been printing the evidence all along: the ring's 127
`ChunkGeneratorStructureState::lambda$generateRingPositions$5` bodies are admitted at t=10.2 s and
executed 60 ms later, taking 10.2 → 73.2 s = **62.9 s** — and 112.7 − 62.9 = 49.8 s is the whole
boot with the lane switched off. So the lane was never slow. **It was eager**, and HotSpot's
0.202 s `Done` shows the JVM does not do that work in this window either.

The cure is to scope the drain to the future actually observed, and to serve the calls whose
handlers already accept a pending source (`allOf` counts pending inputs down; `cf_chain`
registers a continuation) without draining at all. A downstream built over a lane future is
recorded in `LaneBatch::dependents`, so the join that eventually arrives still forces the batch.

## The three things this appendix got wrong

1. **"Per-task nursery spans are the enabling change."** Wrong — PHASE 2 already pre-carves
   per-task slices.
2. **"The blocker is main's GIL-held park, so the enabling change is a suspend/resume
   safepoint."** True as a description of the park, and irrelevant: nothing needed to resume
   early, because the batch should not have been running yet.
3. **"No admission rule can separate the two workloads."** Right, and that is exactly why the
   answer was not an admission rule. The separation is not a property of the TASK — at the
   deferral point the two workloads are identical, as rows 432/433/451/452 correctly found — it
   is a property of the **observation**. `RingLane` joins the futures it deferred, so it still
   drains and keeps its win (4130 ms scoped vs 4501 ms blanket, hash `37539c6c375d0c3`); MC's
   boot touches a chain that does not need them, so the ring stays pending.

⇒ **Four refuted discriminators were four correct answers to the wrong question.** The lesson
worth carrying: when every property of the producer looks identical, look at the consumer.

## And two cuts of the scope were wrong before the third was right

Both were caught by `RAVA_LANE_DRAIN_TRACE`, which prints the draining method and the batch it
forces — the boot has exactly **five** drains, the reload's three through `thenCompose` and the
ring's 128-task batch through `allOf`:

* Exempting the whole chaining family **broke the datapack load** —
  `UnsatisfiedLinkError: invokevirtual Codec.decode (from Codec$RecursiveCodec)`. MC's reload
  builds its stage graph inside the `thenCompose` function; running that later is not equivalent
  to running it then.
* Exempting **only** `allOf` bought nothing: MC calls `allOf(...).thenApply(...)`, so the force
  moved one link down the chain.

So the family is exempt and `thenCompose` is not, and anything unlisted drains — the safe
direction, because being merely slow is recoverable and publishing a half-built graph is not.

## What this leaves

The interpreter half above (the contiguous value stack) is unaffected and still the largest
measured item on the compilers-off benchmark. `RAVA_OFFLOAD_FIRST` is now redundant on the boot:
it was worth 2.13× only because it routed around the lane, and there is nothing left to route
around. `bench/product_soak.ps1` exists because `offload_soak.ps1` runs `RAVA_JIT=0`, where the
lane is not armed at all, so it could never have caught any of this.

*(⚠ 2026-09-16: the contiguous-value-stack sentence above is stale — stage 1 has since
landed in `578247f8`, bit-exact and measured neutral, and the remaining stages-2+3 slice
is closed at 1-2% of the product boot. See the correction notes in the plan section above.)*
