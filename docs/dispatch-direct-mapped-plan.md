# Direct-mapped per-site dispatch table

## Why (measured, 2026-07-28)

Of **489,431,698** dynamic calls out of compiled code on the MC server boot:

| outcome | count | share |
|---|---|---|
| machine-code PIC hit (no Rust callback) | 194,833,724 | 39.8% |
| **PIC miss → `rava_dispatch_miss`** | **289,659,169** | **59.1%** |
| plain invoke → `rava_invoke` (site emitted no PIC) | 4,938,805 | 1.0% |

`invoke` is **89.5% of all ~453M compiled→host callbacks**; `getfield` is a distant second
at 6.7% and nothing else clears 5M. At the recorded ~99ns for a warm host fall, 290M misses
is **~29s of pure transition cost** — larger than the whole ring window, and it explains both
the host-bound character of the boot and why the lane's CPU is superlinear in worker count
(a transition does not parallelize like the compute it interrupts).

Three cheaper explanations are already dead:

* **not a coverage gap** — 65% of the 9542 compiled sites *do* emit a PIC, and the 3270 that
  don't are 2084 static + 1177 special, structurally non-PIC-able and 1% of traffic;
* **not the synthetic-frame gauntlet** — that whole population is 393,018 attempts (96%
  native-served), three orders of magnitude too small;
* **not the lane lock** — `RAVA_LANE_OUTER` at width 6 puts the top holder at `hold_ms=1`,
  ~2ms across the entire top-15.

The 4-way machine-code PIC simply **misses 60% of the time**, and jit.rs's own steady-state
census says why: of 1640 compiled dispatch sites over the ring workload, **79% monomorphic,
14% oligomorphic (2-3), 3% polymorphic (4-15), 2% MEGAMORPHIC (16+)**. Roughly **39 sites**
carry the misses. Coverage is not time.

## What will not work (already built and refuted — do not retry)

* **A wider PIC.** 16+ receiver classes defeats any way count, and a wide PIC on the serial
  path regressed when it was tried.
* **The unbounded freeze-resolve table consulted from MAIN.** RingScan(8) 2778ms → 3795ms,
  **+37%**, bit-exact both ways. Main already has a 16-way per-site direct cache in front of
  the same gauntlet and it serves main's access pattern more cheaply than an open-addressing
  probe. That table earns its keep on a *worker* only because a worker may not fill the
  direct cache.
* **Doing the fallback lookup in Rust.** The callback *is* the ~99ns cost. Any lookup that
  still exits machine code cannot win, however good the data structure. This is also why
  `RAVA_FU_FIELD_IDS`-style memoisation does not transfer here.

## The shape

A **direct-mapped table indexed by arena class id**, probed *in machine code* only after the
existing N-way chain misses. No hashing, no probe sequence, no way limit, and no conflict:
each receiver class owns its own slot, so a megamorphic site stops evicting entirely.

Emitted control flow, spliced between `probe_ways` and the existing `have`/`miss` branch in
`emit_dispatch_pic` (jit.rs ~20938-20963):

```
f4 = probe_ways(...)                  ; unchanged 4-way global / 16-way worker chain
brif f4 != 0 -> decide(f4)
tbl_probe:  tbl = load [slot]         ; slot addr baked as a constant
            brif tbl == 0 -> decide(0)
tbl_bounds: cap = load [tbl + 0]
            brif cid >=u cap -> decide(0)
tbl_load:   f2 = load [tbl + 8 + cid*8]
            jump decide(f2)
decide(fptr):                         ; block param, so `hit` keeps ONE predecessor
            brif fptr != 0 -> hit else miss
```

`hit` is left with a single predecessor deliberately — it already reads `fptr` as a
dominating SSA value and consumes the low deep/leaf bit, so routing the new arm through a
`decide` block param avoids touching that block at all.

### Storage and lifetime

* The per-site box grows `[i64; 8]` → `[i64; 9]`; index 8 holds the table pointer. The
  baked slot address is `cache_ptr + 64`.
* Read the slot from the **baked global `cache_ptr`, never from `cache_eff`** — the worker
  arm's `pt` slice is a 16-way per-thread stride and its index 8 is a real way. The seed copy
  is `IC_GLOBAL_BYTES` = 64 bytes, so it still copies only the 4 ways and the table pointer
  stays per-site global. This is what keeps the worker arm correct for free.
* Table layout `[cap, f_0, f_1, …]`, allocated at **promotion** (site miss count crosses a
  threshold), `cap` chosen from the live class count. Sites that never go megamorphic keep a
  null slot and are byte-identical to today.
* Store **only positive fptrs**. `-1` (NeverCacheable) stays 0 in the table, so those classes
  fall to `miss` exactly as they do now.

### Soundness

Same argument that already licenses `freeze_resolve`: compiled fptrs are immortal (main's
`JITModule` never frees), verdicts are monotonic per (site, class), and 8-byte aligned stores
are atomic — a reader sees either 0 or a complete valid fptr. The table is written only by
`rava_dispatch_miss`, which already resolves and publishes (site, class) → fptr under the
resolve lock. Nothing is carried across a lock release.

## Increment plan (tracer bullet first)

1. Grow the site box to `[i64; 9]`; bake the slot address. No reads yet — machine code and
   hashes byte-identical. Gates must stay bit-exact.
2. Allocate + fill the table in `rava_dispatch_miss` behind a flag, still not read by
   compiled code. Census the fill rate and the class-id range so `cap` is chosen from data.
3. Emit the probe. Gates bit-exact ARMED and DISARMED, then an interleaved boot A/B.

Flag it `RAVA_DISPATCH_TABLE` (opt-in) until it is measured; flip to `RAVA_NO_…` only on an
ON-CLEAN result, per the standing bar (every ON run beats every OFF run, min-of-N, or it does
not ship).

## Gates

ChunkFill `8790f04ace726ecf`; RingScan(8) / RingLane(8) / RingLaneInstance(8)
`37539c6c375d0c3`. RingLane and RingLaneInstance are the only two that ARM the lane, and
RingLaneInstance covers the MH kind 5/7 instance-lambda shape the boot actually uses.

---

## VERDICT: REFUTED (2026-07-28)

All three increments shipped behind `RAVA_DISPATCH_TABLE`, bit-exact on all four gates, 410
tests. The mechanism works and is worth nothing on the clock.

Promotion had to be rewired first. Keying it off evictions in the miss handler's arena fill
promoted 67 of 9542 sites and the probe was reached on **0.54%** of misses, because that fill
runs 4,607 times against ~300M misses. Keying it off the site's own miss count, and mirroring
every freeze-table serve into the table, changed that completely:

| | before rewire | after |
|---|---|---|
| machine-code table hits | 1,512,438 | **35,018,916** |
| real PIC coverage | 39.3% | **45.9%** |
| PIC misses | 300,689,042 | **267,071,199** |

7,247 serve-hook fills produce 35M machine-code hits — the leverage per learned
(site, class, fptr) triple is enormous. And then:

```
done  off 23.574 / 23.942 / 24.452   on 23.975 / 24.094 / 24.236   +1.7%, OVERLAP
wall  off 79.8 / 80.3 / 80.3         on 80.3 / 80.8 / 80.9         ON marginally WORSE
```

**Removing 12.6% of all PIC misses returned zero time.**

### What that invalidates

The case for this lever was `290M misses x ~99ns per warm host fall = ~29s`, and that number
also underwrote the claim that the boot is host-bound *because of dispatch*. Removing 34M
misses should have paid ~3.4s and paid nothing. So either these callbacks cost far less than a
warm host fall at the margin (the freeze-table serve arm is one open-addressing probe plus a
direct call — nothing like a cold fall), or the three loads and two branches this probe adds to
the remaining 267M misses cost back what the 34M saved. The A/B measures net and net is what
matters, so the distinction does not rescue the lever.

**Counting host callbacks is not a way to price host time on this boot.** Any future lever
argued from a callback count needs its price measured, not assumed.

### What is still true

* The population numbers stand: 489M dynamic calls, ~59% PIC misses, `invoke` 89.5% of all
  callbacks, ~39 megamorphic sites, misses 78% arena / 22% heap-Ref.
* `[IC-MISS]` shows only 42.4M of ~300M misses ever reach `dispatch_miss_resolve`, and 97% of
  those are non-arena receivers going straight to `jit_call`. Where the other ~258M are served
  is still not attributed — but since serving them faster demonstrably buys nothing, that
  question is no longer on the critical path to the goal.

The code stays flagged off and inert. The counters are the deliverable.

---

# THE MEASURED LEVER, 2026-07-28: blocks per dispatch site

This document's own lever (a direct-mapped table) was refuted above. The measurements that followed
found where the boot's cost actually lives, and it is still dispatch — but a different property of
it.

## What was ruled out

* **The value-model arc's final flip: ~5.6%.** Priced on `bench/BareRef.java`, built so the flip is
  SOUND (all refs arena-born 0b00, both arms hash-identical) and its hot loop is nothing but
  ref-field access — the most favourable case it can have. `bench/bareref_mix.ps1`.
* **The arena ref-cell classify: 16 of 435 merges.** Folding its pure arms branchlessly
  (`RAVA_JIT_FLAT_TAGSELECT`) moved 852→836 blocks for +390 instructions, and folding the flat twin
  changed *nothing* — because `emit_arena_getfield_ref_flat` is invoked **zero times**.

## What the census actually says

`RAVA_JIT_BLOCK_CENSUS` counts emitter invocations (exact) — `bench/block_anatomy.ps1` and
`bench/guard_census.ps1` characterise the result:

```
emit_dispatch_pic   = 2948 invocations      <- dominant by ~8x
ref putfield        =  720 invocations
arena ref getfield  =    0 invocations      <- dead on this path
```

and the hot method, `DensityFunctions$ShiftedNoise.compute(FunctionContext)D` — four lines of Java:

```
852 blocks · ~8900 instrs · median block 4 instrs · 2555 movq
  24.2% contain a CALL      62.7% branch-only, of which 81.5% are BARE MERGES
```

A guard emits test + two arms + merge; the merges dominate, each carrying block params and dozens
of SSA aliases, and that phi web is the plausible source of the movq volume.

⚠ **Do not quote "20 blocks per site".** That is the static `create_block()` count and an upper
bound: gprobe/wprobe/pmerge need IC_PARALLEL, tprobe/tbounds/tload/decide need RAVA_DISPATCH_TABLE,
sblk/cont need heap_mode 1, refpic/refhit/refcheck heap_mode 2, refdeep ic_deep — all off or
conditional in a default build, where the real figure is nearer 8–10. The invocation counts are
exact; the products are only "which emitter dominates".

## The lever

**Reduce the blocks a dispatch site emits.** Not the value-model arc (5.6%), not the ref-cell
classify (16 of 435). The next step is to count blocks per `emit_dispatch_pic` invocation *in a
default build* — the same counter, incremented per `create_block()` rather than per call — and then
attack whichever arm is both unconditional and mergeable.

★ Method note, paid for about six times this session: every claim of the form "path X is the hot
one" that was reasoned from source was refuted by the next counter (`!initialized`, "nothing
migrates", "Heap::alloc is the choke point", "the compiled path never mints enums", "the hot method
uses the flat emitter"). **Instrument it; do not reason about it.**

## VERDICT: per-site receiver-kind specialisation is REFUTED (2026-07-28)

The lever named at the end of the previous section was made concrete: the heap-Ref arm costs
**6 of the 12 blocks** a dispatch site emits (measured by toggling `RAVA_NO_JIT_HEAPPIC` and
re-reading the per-call count — `bench/dispatch_arm_cost.ps1`), so emitting it only where a
heap-Ref receiver is actually possible was sized at up to half of all dispatch block volume,
~47% of the hot method's 852 blocks. The proof was to come from `SV::arena`, the method-entry
unswitch's compile-time guarantee that a value is a raw arena pointer — already trusted by the
field emitters, so no new soundness argument was needed.

A counter (`block_census::PIC_ARENA_RECV`, incremented at the top of `emit_dispatch_pic` before
anything acts on it) priced the coverage. Both measurements are load-independent — block counts
are a property of emitted code — and both were taken before a line of the emitter was written.

| build | dispatch sites | blocks | arena-PROVEN receivers |
|---|---|---|---|
| default | 2948 | 35,376 (12.0/call) | **0 (0.0%)** |
| `RAVA_FLAT_GETFIELD=1` | 3430 | 41,160 (12.0/call) | **138 (4.0%)** |

**Two independent reasons it dies, either one sufficient:**

1. **The proof is dormant.** `flat_getfield_enabled()` reads `RAVA_FLAT_GETFIELD`, which is
   opt-in and OFF by default, so `arena_proof` is never populated and no `SV` ever carries
   `arena = true`. The same single cause explains the sibling zero in the same census line —
   `arena ref getfield=0` — i.e. `emit_arena_getfield_ref_flat` being invoked zero times, which
   had been recorded separately as if it were its own puzzle. Do not describe `SV::arena` and the
   method-entry unswitch as live machinery; they exist, and they are switched off.

2. **Armed, the coverage is 4% and the arming costs more than it saves.** `flat_unswitch_slot`
   pins ONE slot per method — the one with the most `getfield` reads, ≥3 — and `SV::arena` is set
   solely by the `aload` arms off that slot. Dispatch receivers are overwhelmingly not it. Worse,
   the unswitch emits the body TWICE: arming it moved 2948 sites → 3430 and 35,376 blocks →
   41,160, **+16% blocks everywhere**, to make 6 blocks removable at 4% of sites (≈2% of dispatch
   block volume). That is the codebase's oldest recorded shape — see the CODE-SIZE vs boot lesson:
   boot is compile-once-and-move-on, so a body-DUPLICATING optimisation loses however good the
   local win. Cheapen the operation, never specialise the body.

**Ceiling check on the target itself.** Removing the heap-Ref arm outright — all 6 blocks at
*every* site, far beyond what any per-site predicate could reach — is exactly what
`RAVA_NO_JIT_HEAPPIC` does, and it is on record at **boot −2.6% / FILL +12.7%**. That figure
nets a code-shape gain against a dispatch-behaviour loss, so the pure shape win is somewhat
larger than 2.6% — but it bounds the whole family. Halving dispatch block volume is a
single-digit-percent lever, not the 3.9× single-thread that `wall < 20s` requires.

★ The method note above held for a seventh time, and this instance is the cheapest yet: the
lever was refuted by two counters and one env var, before any emitter change, in the time it
would have taken to write the first version of it.
