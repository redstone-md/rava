# WALL60 Analysis — fix projections and final-result model

Diagnostic-only projection, 2026-08-03. No code changed. Every number is derived
from the measured records in `docs/` (ETW 2026-08-02, RIP round 2, fresh serial
pc-stack 2026-08-03, dispatch-table A/B 2026-07-28). Each projection carries the
refutation caveat from `docs/dispatch-direct-mapped-plan.md`: **counting a hot
path is not pricing it** — treat all of this as bounds, not promises.

## 1. Baseline decomposition

Operative current status (user): `wall ≈ 60s`, `Done ≈ 23s`.
Fresh `release-fat` measurements across the records: `wall 60–80s`, `Done 20–27s`.

```
wall 60s
  ├── serial prefix S (process → Preparing level):   ~37–53s   (ETW: 31s to Environment
  │       + 22s to marker; fresh pcstack: 38s + 23s; user-status implied 37s)
  │       CAVEAT (§10 fact 1): block C is NOT network sleep. Our HTTP stub fails
  │       instantly (`reflect.rs` urlconnection IOException) and the guest fetch
  │       has no retry loop — the yggdrasil error line is a background task
  │       whose delay varies 0–16s (GIL starvation), not a wall cost.
  └── Done window D:                                 ~20–27s   (ring worldgen)
```

ETW (diagnostic run, PID 26580, `docs/lane-locked-residue.md:132`):
- coordinator / serial boot: **56.2 CPU sec** — a single logical thread, sequential.
- 4 ring workers: **21.6–22.0 CPU sec each**, concurrent over ~22s wall → the ring is
  already genuinely parallel at ~4×, `CPU/wall ≈ 4`.
- Coordinator stack: `jit_call` 34.7%, `jit_call_body` 34.6%, `rava_dispatch_miss` 29.8%,
  `invokevirtual` 29.4%, native owner-dispatch ~14.5%, Cranelift compile only ~5.1%.

RIP (round 2, shares only): inside compiled code = A+B **7.4%**, C **5.4%**, ring **37%**,
whole boot **19.3%**. The serial boot is 93–95% host dispatch / GC / allocator / lookups.

## 2. Fix 1 — ring lock removal / lock-free ring execution

`docs/vm-shared-exec-plan.md` model, re-derived on current numbers.
Done-window work ≈ **88 CPU-sec** (4 workers × ~22s), serial residue within Done ≈ **2.2s** (RIP).

```
Done(N) ≈ 2.2 + 88 / N        (N = effective parallel workers)
  N=4  → 24.2s   (matches measured ~23s)
  N=8  → 13.2s
  N=12 → 9.5s    (i5-14400F P-core ceiling)
```

- Best case (lock fully gone, 12 effective workers): **Done 9–13s**.
- Realistic (lock removed but +37%-parallel-loss risk from the ring-parallel
  refutation): **Done 12–15s**.
- **Wall effect: ≈ 0–4s.** Wall = S + D; S is untouched by this fix.
  `S + Done(N=8)` = 37–53 + 13 ≈ **50–66s** → still ~60s. This fix is worth
  essentially nothing on `wall`, everything on `Done`.

## 3. Fix 2 — reduce the serial prefix S (the real wall lever)

S = 37–53s of sequential coordinator guest execution. RIP cluster of A+B (self-time):
dispatch host path 16.8%, interpreter opcodes/frames 16.1%, allocator 11.7%,
name-keyed lookup 6.6%, string 6.1%, thread-local 5.5%, GC 5.3%, compile 3.1%,
compiled code 7.4%.

The three per-block-state loops (~26k states: `WallBlock.lambda$makeShapes$0`,
`StateHolder.lambda$populateNeighbours$0`, `BlockBehaviour$BlockStateBase.initCache`)
plus `MapDecoder.compressedDecode` / datapack are the named serial consumers.

- Forking S: **refuted.** The `initCache` fork admitted 0/712 (irreducible
  `Comparable.equals` / `List.get` interface closures). Cap ≈ 0.
- Speeding S up (lock-free dispatch + GC/allocator + TL, i.e. the ownership
  redesign): the addressable host fraction is ~50–68% of A+B samples. Optimistic
  aggregate 30–40% reduction of S:
  - `S` 37s → 24–30s; `S` 53s → 34–42s.

## 4. Combined projections

| scenario | wall | Done |
|---|---:|---:|
| baseline (user status) | 60s | 23s |
| ring lock removal only | 58–62s | 12–15s |
| serial-prefix speedup only | 42–48s | 20–23s |
| **both, optimistic** | **36–44s** | **9–13s** |
| **absolute best case** (S halved, ring at 12 workers, residue 2.2s) | **~28–33s** | **~9.5s** |

## 5. Honest verdict vs the goals

- `Done < 10s`: **reachable in principle** at ~12 effective workers with the ring
  lock fully removed (2.2 + 88/12 = 9.5s). Marginal, and contingent on beating the
  measured +37% parallel loss.
- `wall < 20s`: **NOT reachable** by any identified fix. The binding floor is the
  serial coordinator prefix S = 37–53s of sequential guest work; even with
  `Done = 0`, `wall ≥ S`. Reaching `wall < 20s` requires S itself to shrink 2–4×,
  and every lever aimed at S so far is refuted (fork 0/712, dispatch-table +1.7%,
  memo hits=0, layout-packing, λ-PIC). The ownership redesign is the only lever
  sized for S, and its optimistic bound lands at ~28–33s wall, not < 20s.

## 6. Key numbers to keep

- 489.4M dynamic calls/boot; PIC hit 39.8%, miss 59.1% (289.7M) — but removing
  34M misses returned 0 wall ⇒ callback counts do not price host time.
- 86.6M resolve-lock acquisitions/region; `worker_direct_hit=0`.
- 374 `&mut Vm` sites; `opcodes::run` needs exclusive `&mut Vm` — the lock supplies it.
- Coordinator = 56.2 CPU-sec serial; ring = 4×22 CPU-sec concurrent; Cranelift ≈ 5%.
- RIP inside compiled code: A+B 7.4%, C 5.4%, ring 37%, whole 19.3%.

## 7. How to push wall lower — levers aimed at S, not Done

The trap in every prior gate: levers were A/B'd on `Done`, which is ring-dominated
and already parallel. S (the serial prefix) is the wall, and it is host-bound.
The following levers target S specifically; three are ALREADY BUILT and
default-OFF, which makes them cheap (flag A/B, no code).

### 7.1 `RAVA_BACKGROUND_JIT` — the strongest untapped wall signal

`docs/copy-and-patch-roadmap.md:211` — one bounded persistent compiler thread;
Cranelift planning stays on the VM thread, only pure codegen moves off-thread.

Measured already, first fresh pair: **wall 70.1s (on) vs 72.2s (off), −2.1s**.
Not promoted because the follow-up measured `Done` (overlap), and `Done` is the
ring — the wrong metric for a serial-coordinator lever.

Why it should win on wall: the 2026-08-03 serial pc-stack shows **`compile` on
14–24% of the coordinator's PC-stack samples in the four serial blocks** (12.7s:
21.4%, 26.1s: 14.4%, 40.0s: 20.2%, 55.4s: 23.6%). That is Cranelift codegen
running on the coordinator's critical path during S. Offloading it to another
P-core is the definition of a serial-prefix win. RIP self-time only credits
Cranelift 3.1–6.1%, but inclusive PC-stack time is 14–24% — the compiler frames
sit on the stack while the coordinator waits, which the RIP self-share hides.

Estimated: reclaim ~10–20% of S → **wall 60s → ~52–56s** (single lever).

### 7.2 `RAVA_CNP` / `RAVA_AOT_CLIF` / `RAVA_LLVM_AOT` — pure-int tiers

`cnp.rs` (copy-and-patch): pure-int static methods run as inline x86-64 from the
interpreter seam (`cnp_run`, lib.rs:18517), measured **2453M op/s vs 182M dispatch
= 13.46× vs interpreter**. AOT tiers run from call #1 (`aot_run`, lib.rs:18529).
All default-OFF.

Caveat: the named serial consumers (`WallBlock.makeShapes`, `initCache`,
`MapDecoder.compressedDecode`) are OBJECT-heavy (Map.get/entrySet bridges), not
pure-int — so coverage on S is likely small. Numeric AOT hits the ring's
DensityFunction.compute, which is not the wall. Low expected value, cheap to test
with `RIP inside compiled code` as the coverage meter.

### 7.3 GC / nursery sizing on S

A+B RIP: allocator 11.7% + GC 5.3% ≈ 17% of S self-time. The serial boot
allocates heavily (registry objects). `RAVA_NURSERY_KB` default is 256MB with
the lane; a larger young gen during the serial phase is a pure config A/B
(GC-stress correctness gate stays `RAVA_NURSERY_KB=2048`). Could reclaim a few %
of S. Cheapest test of all — no code.

### 7.4 Map.get bridge re-A/B on WALL (rejected on Done)

The 2026-07-31 Map.get tracer removed the bridge (native-bridge samples 16.1% →
13.4%) but was rejected because `Done` did not move (off 23.108 / on 23.223).
The pcstack shows ONE Map.get site at **44% of block 12.7s and 31% of block
26.1s**. It was never A/B'd on wall. Per the dispatch-table lesson, "callback
counts don't price host time" — so expect it to fail too, but it is a one-flag
test and directly attacks the single largest serial bridge.

### 7.5 The structural endgame (unchanged)

The only lever sized to cut S 2–4× is the ownership redesign
(`docs/vm-shared-exec-plan.md`): generic lock-free guest invoke/dispatch on a VM
that does not require the coordinator's exclusive `&mut Vm`. Forking S is blocked
at 0/712 by irreducible `Comparable.equals`/`List.get` interface closures.

### Revised wall projection with S-targeting levers

| | wall | Done |
|---|---:|---:|
| baseline | 60s | 23s |
| 7.1 background JIT | **52–56s** | ~23s |
| 7.1 + 7.3 (GC) | **~50–54s** | ~23s |
| + ring lock removal (Fix 1) | **~44–52s** | **12–15s** |
| + ownership redesign (7.5, optimistic) | **~28–33s** | ~9.5s |

`wall < 20s` still requires S → ~10s, which no single lever reaches; only 7.5 has
the scale, and its optimistic bound is ~28–33s. The honest path is: land 7.1
first (built, measured −2.1s, wrong-metric gate), then 7.3, then decide whether
the ownership redesign is worth its risk.

## 8. The wall = 10 s question — physics first, then code

### 8.1 The work budget (CPU-seconds, scaled to production wall)

ETW at 82.4s diag wall: coordinator 56.2 CPU-sec + ring 4×22 = 143.6 CPU-sec total.
Scaled to the production 60s wall (uniform slowdown assumption):

| phase | CPU-sec (diag) | CPU-sec (prod-scaled) |
|---|---:|---:|
| serial prefix S | 56.2 | ~41 |
| ring D | 88 | ~64 |
| **total** | **143.6** | **~105** |

Parallel floor: 105 CPU-sec / N effective cores.
- 6 P-cores: **17.5s**
- 12 threads: **8.75s**

Add the irreducible serial residue (class loading, `<clinit>` ordering, ~2.2s
measured ring residue, dispatch/GC glue that cannot parallelize): the honest
floor is **~12–16s wall on this machine even with every lever perfect**.
`wall = 10s` sits AT the physical edge of a 12-thread machine and BELOW the
floor of a 6-P-core machine. No code change reaches it by parallelization
alone — the workload's total CPU-seconds must shrink.

### 8.2 The real multiplier: per-op cost, not parallelism

The JVM boots the same serial phase in ~10s of mostly single-threaded work.
Rava spends 37–53s there at 93–95% host overhead. The 17× gap is per-OP cost:
- object model (`docs/master-plan.md`): per-object `Vec`s, per-object field
  metadata, `getfield` scans ids, `field_box`/`array_box` side tables — this
  **blocks the JIT from emitting direct field loads** and triples RAM;
- every guest call re-enters host bridges (PIC hit only 39.8%);
- interpreter at ~13M op/s for cold code, cnp (pure-int only) at 2453M op/s.

Cutting per-op cost 2–3× shrinks the 105 CPU-sec budget itself, which is the
only route to single-digit wall: 105/2.5 ≈ 42 CPU-sec → /6 cores = 7s + residue
≈ **~10s**. This is the correct target of code work for wall = 10 s.

### 8.3 Concrete code changes, ranked by wall impact

1. **Flow-sensitive receiver typing in the offload proof** (ledger option (a)):
   proves the ACTUAL narrow receiver at `Comparable.equals` / `List.get`
   (a property value: enum/Integer, not the 840-implementor closure) →
   admits the `initCache`/`populateNeighbours`/`makeShapes` fork, which is
   **51–58% of the serial blocks** (26.1s: 51.5%, 40.0s: 58.8%). Effect:
   S 41 → ~10–15s. Biggest single code change, a compiler-analysis change in
   the offload proof — generic, no names. Gate: bit-exact fork, 127/127.
2. **`RAVA_BACKGROUND_JIT` → default-ON** (already built, measured −2.1s wall):
   `compile` = 14–24% of serial PC-stack blocks (12.7s: 21.4%, 26.1s: 14.4%,
   40.0s: 20.2%, 55.4s: 23.6%) — Cranelift on the coordinator's critical path.
   Effect: 60 → 52–56s standalone.
3. **Ring lock removal / ownership redesign**: D 23 → 9.5–17s (88 CPU-sec /
   6–12 workers + residue). Effect on wall: 44–52s combined with 1–2.
4. **Map.get bridge re-A/B on WALL** — the 2026-07-31 rejection measured `Done`
   only; the pcstack now shows ONE Map.get site at **44% of block 12.7s** and
   31% of 26.1s. One-flag test; per the dispatch-table lesson it may still
   fail, but it is the largest named serial bridge.
5. **Nursery/GC sizing for the serial phase**: allocator 11.7% + GC 5.3% ≈ 17%
   of S self-time. Pure config A/B.
6. **Object-model reform (the 10s enabler)**: per-class field metadata +
   direct JIT field loads instead of per-object Vecs. This is what lets per-op
   cost drop 2–3×; without it, parallelization alone floors at ~12–16s.

### 8.4 Verdict on wall = 10 s

- **Parallelization alone: floor ~12–16s on 12 threads / 6 P-cores.** 10s is
  not reachable by moving work onto cores — there is not enough work left to
  hide the serial residue.
- **Per-op speedup 2–3× (object model + background JIT + bridges): 10s becomes
  the projected floor.** The JVM proves the target is physically there.
- Recommended order: 8.3.2 (already built, free), then 8.3.1 (largest code
  multiplier), 8.3.3 (ring), and treat 8.3.6 as the only path that makes
  single-digit wall real.

## 9. Our per-op vs HotSpot — where exactly we lose (code-level, from JDK sources)

HotSpot sources at `D:\code\jdk`. The template interpreter's getfield
(`src/hotspot/cpu/x86/templateTable_x86.cpp:2632`) does, after the first
execution:

1. **Bytecode rewriting**: `patch_bytecode(Bytecodes::_fast_igetfield)` — the
   field's resolved offset is BAKED INTO THE INSTRUCTION STREAM after first use.
   Every later access is `access_load_at` = **one `mov offset(%obj), %eax`**,
   no cache lookup at all.
2. **Raw stack slots**: the interpreter stack holds untagged 8-byte words;
   an int is a raw `i32` in a slot, no boxing, no tag, no match on read.

Our `opcodes::getfield` (opcodes.rs:2523) pays, per access:

| step | cost |
|---|---|
| `frame.pop()?` | Vec pop + bounds check + `Result` match |
| pinned-handle match + m3shape probe | branch |
| `tlcache::field_get(site)` | **TLS access + FastMap (HashMap) probe with (class-ptr, cp) key — hash computed per access** |
| `get_field_hint` | `objects.get(handle)` table lookup + `ids[hi]==id` compare + `decode_field` kind match |
| `frame.push(v)` | bounds check + copy of a 24-byte `Value` enum |

That is 10–15× the machine instructions of HotSpot's rewritten `_fast_igetfield`
(one mov + one push). And our `Value` enum is 24 bytes (`Arc<str>` payload),
so every arithmetic op moves 24-byte tagged values through a `Vec` with
bounds checks, while HotSpot moves raw 4/8-byte words on a pre-sized frame.

Per-op loop taxes our interpreter pays and HotSpot does not:

| ours | HotSpot |
|---|---|
| `op_checkpoint_due()` — **TLS access per opcode** (lib.rs:973, `OP_COUNTDOWN.with`) | safepoint poll = one register counter / polling-page load |
| 24-byte tagged `Value` on a `Vec` with bounds checks | raw untagged 8-byte slots, SP in a register |
| every getfield: HashMap probe + heap table lookup | offset baked into the instruction stream |
| match-dispatch on a big `match op` | computed-goto style jump table (similar) |

### 9.1 The real structural fix: stop interpreting the serial phase

The JVM boots with C1 compiled code — almost nothing runs the interpreter
after the first few calls. Our serial phase is **93–95% NOT inside compiled
code** (RIP: A+B 7.4%, C 5.4%). Note: pcstack shows the hot guest methods
(WallBlock.makeShapes, initCache) ARE compiled — but they spend their time
falling into HOST bridges (one Map.get site = 44% of block 12.7s), so the RIP
lands in host while the guest caller sits above it. The host time splits into
three attackable populations:

1. **Compile threshold = 512** (`rava.toml [jit] threshold`): the boot is
   compile-once-and-move-on — each method runs a handful of times, never
   reaches the threshold, stays interpreted forever (the 16.1% interpreter
   cluster).
2. **Compile is on the coordinator's critical path**: `compile` = 14–24% of
   serial PC-stack blocks. Synchronous Cranelift per method.
3. **Bridges out of compiled code**: getfield/Map.get/invokevirtual fall
   back to host on 59% of calls (PIC miss). Each bridge is a callback, and
   the callback body does the full gauntlet (tlcache probe, heap table,
   decode).

Fix: background JIT (built, `RAVA_BACKGROUND_JIT`) + a boot-appropriate
threshold, so serial methods compile OFF the critical path and the serial
phase executes machine code like HotSpot does. Serial inside-compiled share
should move from ~7% toward the ring's 37%, collapsing the interpreter cluster
(16.1% interp + 5.5% TLS + part of 16.8% dispatch ≈ 25–30% of serial self-time).

### 9.2 Concrete per-op code changes, ranked

1. **Local countdown instead of per-op TLS checkpoint** (hours, safe):
   hoist `op_checkpoint_due()`'s TLS access into a local counter in `run()`;
   touch TLS only when the local hits 0. Removes a TLS round-trip from EVERY
   opcode. Small but on every bytecode of every thread.
2. **Bake field slots per (method, pc) — the `_fast_igetfield` port**
   (days): precompute `(field_id, slot)` for arena layouts once per method
   into a per-method `Vec<(u32,u32)>` indexed by pc. `arena_getfield` then
   reads `cell[slot]` directly — no tlcache HashMap, no heap table, no id
   compare. The arena object model already has the slot table
   (`nextgen::slot_of`) — the interpreter just isn't using it per-pc.
3. **Raw stack slots in the interpreter** (weeks, the big one): the JIT
   already executes on raw `i64` operand cells (value.rs "shared ABI");
   move the interpreter stack to `Vec<i64>` + a side ref table, exactly the
   JIT's cells. Removes the 24-byte tagged `Value` from every push/pop/arith.
   This is the per-op 2× multiplier.
4. **Background JIT + lower threshold for the serial phase** (free, built):
   compile serial methods off the critical path; the 14–24% compile blocks
   and the 16.1% interpreter cluster both collapse.

### 9.3 Revised math with per-op 2×

Total work 105 CPU-sec × (1/2 per-op) ≈ 52 CPU-sec.
- 12 threads: 4.4s + ~2–3s serial residue ≈ **7–8s wall** — single-digit
  becomes reachable.
- 6 P-cores: 8.7s + residue ≈ **11–13s wall**.

This is the only arithmetic that lands wall ≤ 10s: per-op 2× across BOTH the
serial phase and the ring, plus full parallelization. The JVM proves the per-op
2× is physically available (its boot is mostly compiled, ours is 93% interpreted).

## 10. Parity target — measured JVM baseline on this machine

The goal is not an absolute wall; it is parity with the JVM running the SAME
server, SAME `world_pristine`, SAME affinity 0x0FFF, SAME box. Measured fresh
2026-08-03 (`bench/jvm_parity_1.log` via `bench/wall_budget.ps1` conditions,
Temurin 25.0.3) vs RAVA run 1 of the same script (`bench/wallbudget_1.log`):

```
phase                 JVM        RAVA        gap
start→Environment      ~6.1s      31s         5.1×
Environment→Starting   ~2.0s      21s         10.5×
Preparing level→Done   ~1.0s      24s         24×
wall (total)           9.1s       77s         8.5×
cpu (total)            25.7s      133.4s      5.2×
Done                   0.273s     23.385s     85.6×
```

Decisive facts:

1. **JVM total CPU 25.7s < RAVA serial prefix 52s.** The JVM executes the
   ENTIRE boot with less CPU than our single-threaded phase A+B+C consumes.
   Parity is therefore NOT a parallelization problem first — it is a per-op
   problem: the JVM does 5.2× less total work than we do.
   - **CORRECTION (self-audit, twice):** the 52s serial prefix is a WALL span,
     not a CPU span. The FIRST correction blamed ~15s of block C on a yggdrasil
     NETWORK TIMEOUT — that is wrong, refuted by the logs and the decompiled
     guest code:
       * `YggdrasilServicesKeyInfo.fetch()` has NO retry loop (decompiled: one
         `MinecraftClient.get` call, catch → `Failed to request yggdrasil
         public key` → EMPTY). Our URL stub throws IOException immediately, so
         a fetch never stalls on the network.
       * The delay between `Environment:` and the `Failed to request yggdrasil
         public key` line is variable — 0s (`ah.log`, `abrun_sc_2.log`) to 16s
         (`wallbudget_1/2/3.log`) — and `Environment → Loaded 1470 recipes` is
         a rock-stable 20–22s in EVERY run, including the 0s-Failed ones. The
         error line is the background fetcher task getting the GIL late; it is
         not a wall cost and fixing it changes nothing on the timeline.
       * Block C (Environment → Starting, 21s) is therefore CPU work — recipe /
         datapack loading — done at 10.5× the JVM's 2.0s (table above). Serial
         CPU is ~46–51s (31s A+B + ~15–20s C), not 37s, and NOT 31s+6s. This
         only strengthens fact 1: `25.7 < 46` holds with more margin, and the
         per-op story covers block C too.
   - **P0.5 is CANCELLED** (was: fix the client network stack, ≈ −15s wall):
     there is nothing to fix — the stub already fast-fails and the guest
     already proceeds offline. No wall win exists here.
2. **Parallelism alone cannot reach parity.** Our CPU floor at the 12-P-core
   ceiling: 133.4/12 = 11.1s wall — already ABOVE the JVM's 9.1s, before any
   serial residue. Any plan whose math keeps 133 CPU-sec loses.
3. **The per-op 2× of §9.3 is not enough.** 2× gives 52s + 133.4/2 = 66.7
   CPU-sec → /12 = 5.6s + residue ≈ 8–10s — borderline. Parity (≤ 9.1s, and
   done with margin) needs ≈ **5× per-op reduction** across serial phase AND
   ring, plus full parallelization: 133.4/5 ≈ 26.7 CPU-sec ≈ the JVM's own
   25.7s, then /12 = 2.2s + residue ≈ **3–5s wall**.
4. The 5× is what HotSpot actually does: template-interpreter ~2× (raw
   slots, baked field offsets — §9) × C1/C2 compiled execution ~2.5× (inlined
   intrinsics: Map.get, ArrayList.sort, streams) on top. That is the parity
   program: **every §9.2 lever is necessary, none is sufficient; the aggregate
   is the 5×.**

5. **Block C is fully decomposed (2026-08-03, `bench/BootPhaseC.java` +
   `RAVA_PROFILE_WALL`, 1s windows).** `WorldLoader.load` = 26.15s on a
   diagnostic run; categories avg over 20 windows:
   ```
   JIT_EXEC       36.4%   ≈ 9.5s   codec lambdas 31% (MapDecoder.compressedDecode
                                  11.3%, KeyDispatchCodec.decode 6.7%, ListCodec
                                  accept 4.4% + decode 2.9%, Decoder$1 3.7%+2.2%),
                                  RTree biome build ~14%, cranelift-compile 17.6%
   JIT_CALLBACK   24.5%   ≈ 6.4s   PIC-miss → host dispatch (the known 59%
                                  PIC-miss surface; sites cumulative, dominated
                                  by Map.get/Function.apply/Iterator sites)
   NATIVE_BRIDGES 21.2%   ≈ 5.5s   Map#get 37.4% of bridges ≈ 2.1s, Stream#map
                                  18.5% ≈ 1.0s, String#valueOf 14.6% ≈ 0.8s,
                                  Object#equals 11.4%, Files#newInputStream 9.8%
   INTERP_EXEC    11.4%   ≈ 3.0s   cold once-run decode code
   VM_DISPATCH     6.3%   ≈ 1.6s   resolve/vtable/tier
   CLASS_LOADING   0.2%
   ```
   JVM executes the same load in ~2s (13×). Every row is already on the
   exhausted ledger: initCache fork REJECTED (object loop, not fork-safe),
   codec decode "obvious lever refuted null", compile ~2-3s real (background
   offload UNBLOCKED but neutral on this boot), Map.get funnel measured to the
   guest toString render (memo ACCEPTED, −1.2s Done). **No new goal-sized lever
   exists in block C** — it is the same generic invoke/dispatch/GC host-bound
   surface the ledger's PC-stack revalidation already named.

Revised program for parity (in dependency order):

- **P0 — measure the JVM ceiling once, correctly** (done: 9.1s/25.7s on this
  box; re-run as a gate after each change set).
- **P1 — Fix 1 ring lock removal** (§2): removes the biggest parallel-loss
  term; worthless on wall alone but required to let the CPU reductions show.
- **P2 — per-op 2× (§9.2 1–3)**: local countdown, baked field slots, raw
  stack. Gets us from 133 → ~67 CPU-sec.
- **P3 — compiled serial phase**: background JIT + boot threshold (§9.1/9.2.4)
  so A+B+C run on machine code like the JVM's; this is the step that carries
  serial 52s → ~10s.
- **P4 — JDK intrinsics in the bridge layer** (Map.get, sort, streams): the
  remaining ~2×, and the only lever that attacks the 59% PIC-miss bridges.
- **Gate at every step**: interleaved A/B on the FULL boot, wall + cpu, bit
  hash `8790f04ace726ecf`; no-adapt rule holds — all changes are
  bytecode/CFG/descriptor-level, none reference workload names.
