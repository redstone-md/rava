# Next-agent handoff

Read this before the ledger. Everything here was measured in the session ending at the commit
that added it; nothing is inherited.

## Where the engine stands

| | rava | `java` | ratio |
|---|---|---|---|
| product boot wall | **47.3 s** | 5.8 s | 8.2× slower |
| product boot CPU | 50.9 s | 19.5 s | 2.6× |
| cores | 1.08 | 3.36 | — |
| `Done` | **0.420 s** | 0.200 s | 2.1× slower |
| peak RSS | 1219 MB | 566 MB | 2.2× |
| compilers-off wall | 122 s | `-Xint` 45.2 s | 2.70× |
| compilers-off `Done` | 0.515 s | `-Xint` 1.753 s | **3.4× FASTER** |

⚠`Done` beats `java -Xint` and LOSES to `java`. Do not quote the first comparison against the
second — that error was made repeatedly in the session that wrote this.

**Read the wall column: our compiler multiplies the boot by 2.6× (122 → 47.3), HotSpot's by 7.8×
(45.2 → 5.8).** The distance is lost in the JIT, not the interpreter.

## The instruments, and how to use them

* **`bench/ShapeOr.java` is the sharp probe.** 14 s, exits by itself, bit-exact
  (`SUM=3cfa327f01e0c901`), reproduces `WallBlock.makeShapes` — block A's largest named
  concentration — and is **42× off `java`** where the boot is 8.2×. Ask it in 14 s instead of the
  boot in 50.
* `bench/jvm_cpu.ps1 -Rava` product, `-Rava -NoJit` standing, no flag = `java`.
* `bench/wt_gates.ps1` — four bit-exact gates. `bench/sem_all.ps1` — 18 differential probes.
* `bench/product_soak.ps1` — soak with compilers ON (`offload_soak.ps1` runs `RAVA_JIT=0`, where
  the deferral lane does not exist, so it cannot see lane changes).
* `bench/NativeCost.java` — prices one native bridge call.
* Profiling: `RAVA_PROFILE_PC=1 RAVA_PROFILE_PC_ALL=1` plus **`RAVA_PROFILE_PC_MS`** (default
  30 s — it never fires on a short probe) and `RAVA_PROFILE_PC_US` (default 10 ms — too coarse for
  a sub-second window). Periodic dump prints `[PC-OPS] [PC-ARM] [PC-CB] [PC-DT] [PC-DTR]`.

### Instrument traps that each cost a wrong published conclusion

1. **`contract_stat` counts NOTHING without `--features contract-stat`** — env var alone is
   silent. "Fewer than 5,000 calls" was published off that silence; the real figure was 400,000.
2. **A flag that changes no behaviour measures nothing.** `RAVA_JIT_NS_BYTECODE`'s first cut fell
   through to a predicate with the same substring test and was inert; it nearly produced a
   refutation of a mechanism that had never run. Verify the behaviour moved before believing an A/B.
3. **The wall sampler's categories disagree with RIP by 4-5×** (JIT_EXEC 52.3% vs 11.3% on the same
   window). Prefer RIP.
4. **The dispatch-table counters are not self-consistent** — a boot reports `serves=0` alongside
   `fills=3233`, and all refusal clauses read zero with `ADMITTED=0`. Resolve before using.

## What the profiles say (five of them, all flat)

Block A (32 s), the datapack reload (16 s), the `Done` window, ShapeOr and two microbenchmarks:
**~91% of time in host Rust, ~8.5% inside compiled code, and no function above ~4%.**

* Block A categories: NATIVE_BRIDGES 27.7%, JIT_CALLBACK 22.5% (pure dispatch tax — the callee's
  own tier overrides the category), INTERP_EXEC 9.3%, VM_DISPATCH 3.4%.
* Block A's bridges concentrate: **four call sites hold 68%**, all block-state machinery.
* The reload's bridges: `Stream#map` ~27%, `Map#get` 15.6%, `Reference2ObjectMap#entrySet` 11.8%.
* `bench/NativeCost.java`: our `java.util` costs **200-600× HotSpot per call** — `Map.get` 820 ns
  against 2 ns. Routing is only ~3% of block A, so that is the natives' own work.
* **91.4% of PIC misses are ARENA receivers**; 38% of host excursions are capturing-lambda calls.

## The next step, sized honestly

A **per-site capture-unpacking thunk**, served from the 4-way PIC (not the dispatch table).

* Why it is needed: `note_serve_direct` refuses `entry.cap_n != 0`, and `emit_lambda_key_select`
  keys by receiver IDENTITY, which can never hit for a capturing lambda. So every capturing lambda
  takes a host excursion.
* Shape: compile one small body per lambda site whose signature matches the buffer the hit arm
  already produces (`[recv, args…]`); it loads `cap_n` captures from the receiver's arena cells per
  `cap_ref_mask` and jumps to the impl. The PIC entry points at the THUNK, so the hit arm needs no
  capture logic. `DirectEntry` already carries `cap_n`, `cap_ref_mask`, `site_key`.
* ⚠**Serve it from the PIC, never the table.** The table indexes by class id, one capture-shape
  class is shared by many sites, and widening the key to the site is **3.3× SLOWER** — measured:
  `RAVA_LAMBDA_SITE_CID=1` reads 35.3/36.0 s on ShapeOr against 10.5/11.0.
* **Worth ~5% of the probe and 2-3% of the boot.** Use the RIP number (13% of the main thread is
  the whole dispatch machinery; capturing lambdas are 38% of it), not JIT_CALLBACK's 28.7%.

## Do not re-derive these (each was built and measured)

* Identity map keys by class contract — killed the boot twice; the engine has two representations
  of every object, so a structural key matched across a `Value::Obj`/`Value::Ref` seam that an
  identity key cannot.
* Cached field ids on the hot map path — microbench 864→820 ns, boot unmoved; block-state maps take
  the r2omap identity scan and never enter that prologue.
* **790,000 interpreted method entries converted to compiled = ZERO.** Short bodies reached through
  a host bridge pay for the INVOKE, not their opcodes.
* Inline depth 8/16/24 — flat. An interface receiver gives the inliner no devirtualised target.
* `RAVA_LAZY_STREAMS` — +1% worse on today's engine.
* Offering `Objects.equals` to the collection native before the guest — +15.7%.
* Turning the dispatch table off — wall-neutral; it costs ~250 MB of RSS and hits 3.0%.

## Non-negotiable

Every change: `ChunkFill 8790f04ace726ecf`, `RingScan`/`RingLane`/`RingLaneInstance`
`37539c6c375d0c3`, 427 tests, 18/18 probes, and a product soak for anything touching the lane,
the GC or dispatch. Measure with ABBA on one binary and a flag; wall on this box is ±8% and CPU
±2.5%. Snapshots are forbidden as a speed path.
