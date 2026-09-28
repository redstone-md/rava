# Block D — the one anomalous window, measured

`Preparing level "world"` → `Done (`. Everything here is from boots taken on an idle machine with
`bench/boot_pc.ps1`, `bench/refusal_census.ps1` and `bench/fu_census.ps1`; ledger rows 223-228.

## Why this window and not the others

⚠ **Read the columns correctly.** The rava column below is the SHIPPED configuration — `RAVA_JIT=1`
plus tier-0 and the two natives — and the `-Xint` column is HotSpot with its compiler off. This is
our full engine against their crippled one, so it is **not** an interpreter comparison and nothing
here says our interpreter is near HotSpot's (it is 6.0× per opcode; see
`docs/interpreter-parity-plan.md`). What the table legitimately shows is WHICH WINDOW is anomalous:
in block D our engine *with* a compiler loses to a JVM *without* one.

Per block, from the guest's own log stamps:

| block | rava | `-Xint` | |
|---|---:|---:|---|
| start → `Environment:` | 18 s | ~37 s | rava **0.49× — twice as fast** |
| → `Loaded N recipes` | 12 | 8 | 1.44× |
| → `Preparing level` | ~1 | 1 | ≈ |
| **→ `Done (`** | **16** | **2** | **7.9×** |
| total | 46 | 47.9 | 0.96× |

The tree's headline — "rava with its JIT is level with HotSpot without its compiler" — is a TOTAL
made of two large offsetting differences. There is one anomaly, not three equal blocks; the "three
equal blocks" reading came from comparing against the *compiled* JVM.

## What the window is made of

Host table, parked samples excluded, enumerating sampler armed:

| row | share of live |
|---|---:|
| `Vm::jit_direct_invoke` | 13.3% |
| `Mutex::lock_contended` | 9.5% |
| `RtlWaitOnAddress` | 5.0% |
| `jit::resolvelock::resolve_lock_acquire` | 4.8% |
| `Vm::jit_direct_probe` | 4.1% |
| `rava_dispatch_miss` | 3.9% |
| `rava_fu_invoke` | 3.3% |
| worker-direct TLS map / `fu_double_view` | 2.7% each |
| `jit_fu_invoke` · `forward_if_moved` · `jit_direct_call_worker` | 1.7 / 1.6 / 1.4% |

**Dispatch cluster ≈32%. Lock cluster ≈19%. Functional-interface cluster ≈7.7%.** About 3.1 of 12
armed threads are live. Only 26-30% of live samples are inside compiled machine code.

The window is not spent executing the guest's worldgen. It is spent deciding where calls go, and
waiting for one mutex.

## The populations, named

- **Locked callees**: `Predicate#test` 412,624 · `Function#apply` 412,623 ·
  `AtomicLong#compareAndSet` 205,051 · everything else under 900. `wsnative_hit` equals the third
  row exactly, so the real locked traffic is **825,247 guest-lambda calls a boot** and nothing else.
- **Freeze table** (the workers' lock-free resolve cache): `table_hit=0 direct_hit=0 stored=6,225
  miss=2,063,140 neg_hit=412,627`. It matches — but every match is a permanent `NeverCacheable`.
- **Why `NeverCacheable`**: `native_served_deep` 87%, `is_cache_class` 11%, `force_native` 2%,
  and `java/*` and `$*` **zero** — which killed the hypothesis that lambda receivers were being
  refused by the `$Lambda` name test.
- **Which method**: `isEmpty()` on four native-served collections = **893,041 calls a boot**, plus
  `GuavaLoadingCache#getUnchecked` 254k and `LinkedTreeMap#get/put` 269k.
- **Dispatch-miss split** (same boot): `dmiss_res=28,707,339` · `dmiss_recv=253,711` ·
  **`dmiss_mega=0`**. No site is megamorphic — which is why PIC WIDENING was refuted (ledger row
  163) — and receiver-class mismatch is under 1% of the misses. The 28.7 M are resolution probes,
  6.8% of all invoke callbacks, and they are dominated by the native-served collection population
  above rather than by lambdas. **That bounds what a lambda-only PIC can reach**, and it is the
  reason to measure the flag rather than argue about it.
- **Callback shape**: `invoke=422,254,784` host callbacks a boot, rising ~50 M per 5 s window
  inside this one. ⚠ Quoted as a shape only — ledger row 176 converted 35 M callbacks into
  machine-code PIC hits, moved the hit rate 39.3→45.9%, and measured **zero**. *Counting host
  callbacks is not a way to price host time.*

## The arithmetic that matters

`Done` is ≈15 s and block D's call-boundary share is ≈8 s of it (51% of 16 s, PC-sampled, i.e.
time rather than counts). **Removing most of that lands `Done ≈8` — under the goal.**

`wall<20` does not follow. Blocks A+B+C are 30 SERIAL seconds in which rava is already twice
HotSpot's interpreter, and summing every open lever at its theoretical maximum still lands at
wall ≈32 (ledger row 223). The two goals have different answers, and the honest statement is that
one of them has no measured path.

## The interpreter is worse than HotSpot's, and it is not what this window is bound by

Both of these are measured and they do not conflict:

* rava's dispatch LOOP is **6.0×** `java -Xint` per opcode and **10.4×** per call
  (`docs/interpreter-parity-plan.md`). HotSpot's interpreter is hand-written machine-code
  templates; ours is a Rust `match`. That gap is real and it is closed only by a template
  interpreter.
* On the REAL boot, rava beats `java -Xint` overall (46 s vs 47.9) and is **twice as fast** in the
  largest block, because the engine is not the loop: the JDK is Rust natives here, and hot guest
  code is compiled.

The sampler puts `INTERP_EXEC` at ~11% of the boot. So "the interpreter is slow" is true, and it
is not the term that decides `wall` or `Done`. **The term that decides them is the call boundary
in this window.**

## Priced, so nobody re-derives them

- lock-free entries for the native-served `isEmpty`/`size`/`get` population: **~0.3-0.8 s**.
  `rava_dispatch_miss` is 3.9% of the window and `jit_call`/`jit_call_body` under 1.5% combined.
- `Value` at 16 bytes: ~5-10% of the interpreter loop, ~500 sites, and it does not touch this
  window (26-30% compiled here, and the loop is not what block D runs).
- the interpreter arc as a whole: closed at 6.0× of `-Xint`, worth −4.5% of boot wall.
