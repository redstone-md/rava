# The JIT is worth 3x, and its tiering is a NARROW optimum

*2026-08-26*

Prompted by a fair question: if the compilers-off standard benchmark runs in tens of seconds,
is the JIT needed at all? Measured on the product boot, same script, same box:

| configuration | wall | CPU | `Done` |
|---|---|---|---|
| **JIT on (default, threshold 512)** | **40-43** | 43-46 | ~0.40 |
| **JIT off entirely (`RAVA_JIT=0`)** | **121.2** | 119.6 | 0.539 |
| JIT on, threshold 8192 (little compiles) | **336** | 334 | never reaches Done |
| JIT on, threshold 32 (much more compiles) | **400** (timeout) | 398 | never reaches Done |
| JIT on, `enable_verifier` forced on | **400** (timeout) | 185 | never reaches Done |

So: **the JIT earns 3x on the product boot**, and the "36 s" figure belongs to the
compilers-off STANDARD BENCHMARK, which is a different workload.

## The part worth keeping

**Both directions off the default collapse, and both land WORSE than having no JIT at all.**
That is not a tuning curve, it is a cliff on either side of a narrow optimum.

The profile says what the collapsed regime is doing, and it is not executing the guest program:

```
threshold 8192          regalloc2::ion::run 8.8% · ArenaMemoryProvider 5.7+4.6%
                        ipnsort 3.4% · Elaborator 2.2% · ZwProtectVirtualMemory 1.8%
threshold 32            ArenaMemoryProvider 11.3+4.3% · regalloc2::ion::run 10.3%
```

~28% of samples inside Cranelift, in a configuration that is supposed to compile LESS. The
mechanism is a feedback loop: fewer methods compiled by entry count → more dispatch misses →
more `force_compile` → the compiler saturates → the boot stops progressing. At threshold 32 the
boot compiled **532 methods in 150 s**; at the default it compiles **1216 in 40**.

Compile cost is not the driver, and that was checked rather than assumed: `RAVA_JIT_OPT_NONE`
nearly halves Cranelift time (0.69 s per 500 methods against 1.30) and the collapsed boot still
times out.

## What was fixed

**Cranelift's verifier was running in production.** It re-typechecks every function the JIT
emits — a compiler-development self-check. It showed as `Verifier::run` at **4.3% of all
samples** in the threshold-32 profile, and turning it off cut cumulative Cranelift time for 1000
methods from **2.64 s to 2.06 s (-22%)**.

The boot itself does not move: compilation is ~3 s of background work in a 40 s boot, so a 22%
cut there is ~0.6 s, under the instrument floor. It is kept because it is free and correct, and
because it makes the compile path cheaper for any future attempt to raise coverage.
`RAVA_JIT_VERIFY=1` restores it — which is the switch to reach for when a codegen change starts
emitting wrong machine code.

## What this says about coverage

Only **1216 methods** compile on a boot, against a `jit_cap` of 4096 — so the cap never binds,
and the A/B that lifted it to 65536 measured nothing (+5.49%, not disjoint, as expected of a
no-op). Only **9.1% of samples land inside compiled code**.

Raising that is the one structural lever left, and it is currently unreachable: every attempt to
compile more collapses into the saturation regime above. The obstacle is the cost of ATTEMPTING
a compile — the shared module's `ArenaMemoryProvider` is 15.6% of the collapsed profile, and the
module is taken, appended to and re-protected per attempt, including for the emit bails the code
itself calls "the common `?` return".

So the next thing worth building here is not a better code generator. It is an attempt path that
costs nothing when it fails.

## The attempt path, and one refuted fix

The obstacle to more compiled coverage is now named exactly: **`finalize_definitions()` is not
incremental.** cranelift-jit re-protects every region the module owns on each call, and it is
called **per compiled method**, so the Nth compile walks N regions and the total is quadratic.
That is the `ArenaMemoryProvider` 11.3% + 4.3% and the `ZwProtectVirtualMemory` in the collapsed
profiles — and it explains why compiling MORE gets superlinearly worse rather than linearly.

⛔**Rotating to a fresh module every 64 definitions does not work.** It bounds the walk, and it
breaks the program: compiled bodies call each other DIRECTLY, and a symbol defined in a retired
module is not visible from the next one. One run was enough — `ringscan`, `ringlane` and
`ringlanei` all MISMATCH and the boot never reaches `Done`. Reverted; the reasoning is left in
`module.rs` beside the guard so the next attempt starts from the right problem.

Doing it properly means declaring cross-module callees as imports and resolving them against the
retired modules' finalized addresses — linkage work, not a batching tweak.

## Two more refutations, and what the cliff actually is

⛔**`finalize_definitions` is not the cost.** It is not incremental, so it read as the term that
grows with the number of compiled methods, and a batching redesign was about to be built on that.
Timed: **0.01 s of 2.23 s over 1000 compiles = 0.4%.** The `ArenaMemoryProvider` weight is
code-space ALLOCATION inside `define_function` — compiling itself.

⛔**Rotating to a fresh module per batch breaks the program.** Compiled bodies call each other
DIRECTLY, and a symbol defined in a retired module is not visible from the next one — `ringscan`,
`ringlane`, `ringlanei` all MISMATCH on the first run. Doing it properly is cross-module linkage,
not batching.

⛔**And the cliff is not synchronous compilation either, which took two attempts to establish.**
`force_compile` does compile inline (it sets `Cold(threshold)` and calls `jit_run` under
`COMPILE_ONLY`), and it also WAITS when it finds a method already `Compiling`. Both were made
asynchronous behind a `COMPILE_ASYNC` request mode — and the boot still timed out at 240 s with
408 methods compiled. Reverted: an unmeasured code path is worse than none.

### What the cliff IS

| configuration | compiled methods | wall |
|---|---|---|
| JIT off entirely | 0 | **121 s** |
| JIT on, threshold 8192 | 408 | **240+ s** |
| JIT on, default | 1216 | **40 s** |

**408 compiled methods are worse than zero.** That is the result, and it survived both attempted
explanations. The reason has to be the boundary: with nothing compiled every call is
interpreter-to-interpreter, and with a little compiled the hot methods run compiled while their
callees do not, so nearly every call CROSSES. The crossing costs more than either side of it.

That is a sharper statement of the seam than the census gave (13.5M callbacks, ~14% of the boot):
the seam's cost is not linear in crossings, it dominates whenever coverage is partial. The
default 512 threshold is past the bad zone; raising or lowering the threshold walks back into it,
which is why both directions collapse.

It also says what raising coverage would require: compiling in dependency order so that compiled
code calls compiled code, or making a crossing cheap. Neither is a tuning change.

## The seam, attributed by site

Two counters that had no reachable reporting point before — the boot-dispatch census only prints
on a graceful exit, which the harness never reaches — are now on the periodic profile dump:

```
[DISPATCH-TOTALS]   interpreted-entries=3,196,280  jit-entries=5,675,530
                    host-crossings=31,439,022      crossings-per-jit-entry=5.54
[RESOLVE-VERDICTS]  compiled=5,353  never-cacheable=8,244  not-yet=6,992
```

**31.4 million crossings against 20,589 resolves.** The resolution is memoised and cheap; what is
paid 31 million times is the CROSSING itself. And the verdicts say who is on the far side:
**8,244 call sites are marked never-cacheable** — the `cls.starts_with("java/")` rule — and they
cross on every call, ~3,800 calls each.

So the ~14% seam is now attributed to a specific, enumerable population: compiled code calling
`java/*` natives through the interpreter's argument protocol.

⚠**And the obvious lever against it is already refuted.** Inlining the LARGEST single callee —
`List.get`, 10.37% of all callbacks — so that those crossings never happen measured **+1.68% and
+3.90%** on the boot. Removing crossings for the top callee did not pay, which is evidence
against a typed native ABI paying either, at least in proportion to the crossings it removes.

The one place the seam demonstrably dominates is PARTIAL coverage (408 compiled = 240 s against
121 s with none), and the shipped configuration is already past that zone.

## The last unpriced lever, priced

Dependency-order compilation — compile callees before callers so compiled code calls compiled
code — was the one idea left with a mechanism and no number. It has a ceiling, and the ceiling is
measurable: of the crossings compiled code makes, how many target a callee that HAS bytecode?

```
32,049,770 crossings
   java/* callee   19,051,911   59.4%   no bytecode exists — nothing to compile
   guest callee    12,997,859   40.6%   could become a direct call
```

**So the scheme caps at 40.6% of the seam.** At the seam's ~14% of the boot that is **5.6% in the
theoretical maximum**, where every guest crossing becomes a direct call — which it will not,
since a virtual site with several receiver classes still needs a check.

That is below the bar for building it, and it closes the last lever that had a mechanism without
a number. The 59.4% majority is reachable only by the two routes already refuted: `java.util` as
guest bytecode (**+8.46%** on the boot) or a typed native calling convention (argued against by
the `List.get` inline measuring **+1.68% / +3.90%**).
