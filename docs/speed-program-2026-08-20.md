# The speed program, re-derived from measurement (2026-08-20)

Written after a session that refuted two arcs the tree was about to spend weeks on, and
re-took every number the remaining plans rested on, shipped one, and closed six. Ledger rows 298-307.

Everything here was measured on a **loaded box** (queue 47.5, floor ~+90%). That is stated
once and applies throughout: **shares, ratios and counts are used; absolutes are not.**

## What is settled

### The interpreter is not the product (rows 298-299)

A product boot to `Done` interprets **10 million opcodes on all threads** — counted, not
sampled, by `OPS_ALL` in `opcodes/checkpoint.rs`. At the measured 4.35 ns per dispatch that is
**~43 ms of a 75-second wall**.

`examples/slotbench.rs` then priced the interpreter's own shape, four machines in one binary:

| machine | slot | dispatches/iter | ns/iter | ns/dispatch |
|---|---|---|---|---|
| A | the engine's `Value` | 8 | 34.8 | 4.353 |
| B | 8-byte untagged cell | 8 | 30.2 | 3.779 |
| D | register IR | 6 | 22.2 | 3.705 |
| C | register IR, fused | 2 | 5.48 | 2.738 |

Untagging is **1.15×**; cutting dispatches 6→2 is **4.06×**; `java -Xint` pays **0.59 ns** per
dispatch against our 4.353. All three facts are real and none of them is worth 0.06% of a boot.

⇒ **Closed as product work:** threaded dispatch, a register IR, an untagged operand slot, and
the whole arc of ledger rows 183-222. ⇒ **Closed:** the INTERPRETER half of
`docs/value-model-rewrite-plan.md`.

### Where the boot actually is (row 300)

`release-diag` + `wall-profile`, one boot to `Done`, 6,000 samples at 10 ms:

| category | share |
|---|---|
| JIT_EXEC (Cranelift machine code) | 43.9% |
| NATIVE_BRIDGES (our Rust JDK) | 25.0% |
| JIT_CALLBACK (PIC miss → host dispatch) | 16.8% |
| INTERP_EXEC | 10.1% |
| VM_DISPATCH | 3.3% |
| CLASS_LOADING | 0.9% |

`INTERP_EXEC` is **not** dispatch — the population above bounds dispatch at 43 ms, so this row
is the work the interpreted opcodes' CALLEES do, billed to the category `run()` armed.

**Biggest named leaf in the entire profile is 8.4% of wall** (`BlockStateBase.initCache`), and
**39.3% of JIT_EXEC has no attribution at all**. The profile is flat, for the fifth independent
time.

### The escape population is three times what the tree recorded (row 301)

`RAVA_JIT_PIC_PROBE` on today's binary:

```
dynamic calls out of compiled code = 452,572,159
  machine-code PIC hit (no Rust)     220,582,632 (48.7%)
  fell to Rust host                  231,989,527 (51.2%)
    PIC miss -> rava_dispatch_miss   227,324,316 (50.2%)
    plain invoke -> rava_invoke        4,665,211 ( 1.0%)
```

Row 244 recorded 148.9 M calls with 27.0% falling to the host and priced the dispatch table
against **38 M** misses. The real denominator is **232 M**, and it is what JIT_CALLBACK's 16.8%
(~13 s of a 79 s wall) is made of: ~57 ns per escape.

`jit/callbacks/dispatch.rs` already says what these escapes are: *"the answer is already known
here — `fill` is a compiled fptr — so the whole cost of these misses is the machine-code EXIT
taken to come and read it."*

## The three levers, in the order the evidence supports

### 1. Stop leaving machine code to be told an fptr — up to −16.8% of wall

232 M escapes a boot whose answer is a compiled function pointer. The mechanism to serve them
exists (`jit/dispatch_table.rs` fills a per-site class-id-indexed table; `emit/dispatch_table_probe.rs`
reads it after the N-way PIC chain comes up empty, so a PIC hit pays nothing).

⚠ **It is default OFF and switching it on REGRESSES the boot**: same-binary flag A/B, E-cores,
strictly alternating, 3 clean pairs — `Done` **+6.58% mean, 3/3 pairs worse**, min-of-N +4.34%
(row 301).

The first hypothesis for why was `note_serve`'s bail —

```rust
if recv == 0 || (recv as u64) >> 40 != 0 {
    return; // not an arena object: no class id to index by
}
```

— i.e. that a heap handle's class key is an interned `Arc<str>` pointer rather than a dense id,
so the handle heap structurally excludes handle receivers from the only mechanism that can serve
the escapes. **The census refuted it** (row 302):

```
mc_hits=43,757,168  mc_probes=43,886,419  serves=8,148  promoted_sites=99
miss_shape[null=1800  arena=228,225,762  heapref=27,346,285  boxed=253,630]
```

Misses are **89.2% arena** — the shape `note_serve` already accepts — so the handle-heap bail is
worth 10.7%, not the majority. Where the table exists it hits **99.7%** of its probes. The defect
is COVERAGE: the probe reaches a promoted site's table on 43.9 M of ~256 M misses (**17%**) while
the other 83% pay its early-out.

**The cause is a dead teaching arm.** `note_serve` has one call site, inside the freeze-table
serve arm, and both boots read `freeze: table_hit=0 direct_hit=0` on the same line that reads
`callbacks: … direct=238,173,575`. The table is taught by a path with zero traffic and starved on
the one carrying the population.

⛔ **The obvious fix is unsound as written.** Mirroring `DirectEntry.fptr` publishes a pointer
with the wrong calling convention: the machine-code hit arm calls `f(buf, ctx)` with the compiled
caller's buffer as-is, while a `DirectEntry` is entered through `jit_direct_invoke`, which
re-marshals for `buf_len`, `wide_mask` pad slots, `shift` and `arg0_guard`. That is a silent
wrong-method dispatch, not a crash.

### ✅ DONE — `Done` −20.5%, commit `156035d`, ledger row 303

Two hooks, 166 lines, default-ON with `RAVA_NO_DT_LEARN` as the kill switch:

* `note_miss_site` counts promotion on EVERY miss, not only inside the freeze arm;
* `note_serve_direct` publishes the worker direct cache's verdict into the site's table.

The admission comes from `DirectEntry`'s own fields — `leaf` → `fptr|0`, `self_reg` under
`ic_deep_enabled()` → `fptr|1` with the `buf_len <= dispatch_callbuf_slots()` precondition
re-checked, any lambda machinery refused. **Wide parameters are admitted deliberately**: the table
probe runs inside the PIC on the SLOT buffer, unlike `freeze_resolve::put`'s packed one, and the
ring's hottest callee `ImprovedNoise.sampleAndLerp(IIIDDDD)D` is one of the 17 sites the freeze
table declines.

| | before | after |
|---|---|---|
| machine-code PIC hit | 214.9M (49.0%) | **361.3M (86.8%)** |
| fell to the Rust host | 222.9M (50.9%) | **54.5M (13.1%)** |
| worker-direct answers | 200.7M | **32.1M** |
| promoted sites | 99 | 899 |

Same-binary flag A/B: `Done` **−20.5%** min-of-N with **every OFF below every ON**, 0/4 pairs
favour disabling it; wall −8.5%, also disjoint. RingScan **14/14** `37539c6c375d0c3`, ChunkFill
**12/12** `8790f04ace726ecf` and level — the effect scales with SITE DIVERSITY (99 → 899 promoted
sites), which a gate does not have. `cargo test --release --features next-gen-gc`: 417 passed,
0 failed.

**What is left in this lever:** of the remaining 54.5 M escapes, **27.1 M carry a heap-handle
receiver** and are structurally unindexable — a handle's class key is an interned `Arc<str>`
pointer, not a dense id. That is now the majority of what remains and is worth ~2% of wall, so it
is a later item, not the next one.

### 2. The value model, in COMPILED code — the half that survived

`Ap2.compute` compiles to 334 loads and 263 tag operations around ~8 multiplications. JIT_EXEC
is 43.9% of wall. This is `docs/value-model-rewrite-plan.md`'s host/compiled half, and lever 1
shows it is not only a per-instruction tax: the handle representation is what disqualifies half
the dispatch population from ever being served in machine code.

### 3. Compile the JDK instead of emulating it — the long one

NATIVE_BRIDGES is 25% of wall, **104 distinct classes answer native calls** (~10 M entries a
boot: `ArrayList` 3.37 M, `StringBuilder` 1.12 M, `Class` 0.75 M, `Boolean` 0.70 M,
`Iterator` 0.66 M, `HashMap` 0.65 M), and `vm/jit_run.rs` refuses to compile any of them by
construction — so a compiled caller can never inline one, which is why the inline-depth cap
measured flat at 2, 3, 8 and 12 (row 259).

HotSpot boots the same jar in ~21 CPU-seconds against our ~78 because its JDK is compiled Java
that inlines into its callers. The incremental form of this lever is already precedented:
`emit/collections.rs` emits an arena `HashMap.get` probe entirely in machine code, and
`emit/intrinsics.rs` recognises callees **by bytecode shape**, never by name — which is how this
stays inside the no-adapt rule.

## Closed the same day, each with a count or a paired A/B

Six directions were shut cheaply. They are listed because a closed direction is worth as much as
an open one and costs a later session nothing to re-derive.

| direction | verdict | evidence |
|---|---|---|
| interpreter dispatch | **0.06% of the product** | 10 M opcodes a boot, counted (row 299) |
| JIT coverage | not refusal-bound | `refused=1`, `gate_native_served=0` in a whole boot (row 306) |
| inline DEPTH | flat | row 259, 2/3/8/12 within ±1.4% |
| inline SIZE cap | flat | 35 → 120, +0.42%, 2/6 pairs (row 306) |
| `fastutil_array_get` for `Map#get` | not its population | 1,005,661 receivers, 786 fastutil (row 304) |
| native array loads | **load-bearing, +70% to remove** | `RAVA_NATIVE_LOADS=0`, 0/6 pairs (row 305) |
| shared out-of-bounds arm | **−11% instructions, zero time** | RingScan −0.54% at 4/6 against; boot mixed (row 307) |

## What the session taught about method, which outlives the numbers

⭐**Three times a real mechanism failed to become time, and only the paired A/B told them apart.**
227 M PIC misses were real and the table did not serve them (fixed, −20.8%); 92 method exit points
are real and lie on cold paths (reverted); 1,005,661 map lookups are real and reach an arm that is
not theirs (no change). Reasoning ranked all three the same.

⛔**Twice the instrument was not connected and its reading still looked sane.**
`RAVA_DISPATCH_TABLE` is read by NO code — an A/B against it compared two identical
configurations and printed "+6.58%, 3/3 pairs". `freeze: table_hit=0` was an unarmed counter, not
a dead arm. Both conclusions happened to survive, which is luck, not method.
⇒ **`grep` a flag in `crates/` before an A/B; arm a counter before believing its zero.**

⛔**Once a recorded measurement had expired.** Memory said `RAVA_NATIVE_LOADS=0` was 1.7 s FASTER;
today it is +70% on fixed work. That number was taken on a configuration being used to work
around a native-IALOAD miscompile which has since been fixed.
⇒ **A workaround's measurement expires with the bug it was working around.**

⛔**And instruction counts do not price a codegen change in either direction.** The tree already
knew body-DUPLICATING optimisations lose (`docs/` code-size lesson); row 307 adds that
body-SHRINKING ones do not win: −11.0% instructions, −27.4% calls and −26.6% stack traffic in the
ring's hottest method measured as zero on two instruments.

## The arithmetic, stated honestly

Lever 1 is **banked at `Done` −20.8%** and the dispatch escape population is down to ~5% of wall,
so what remains of it (27.1 M heap-handle receivers, unindexable without a dense class id) is a
~2% item behind the value model rather than a lever of its own.

That leaves the two big categories, and neither is a point lever:

* **`JIT_EXEC` ~34%** — the value model in compiled code. Its prize is MEASURED, not inferred:
  `RAVA_JIT_BARE_REF_LOAD` is **2.43×** on ref-deref-dense code with the control at exactly 1.00×.
  The flip residue is already down to 3-305 cells, so the blocker is not coverage arithmetic — it
  is stale receivers under GC. **Start that arc at the barriers, not at the emitter.**
* **`NATIVE_BRIDGES` ~29%** — of which streams are the largest cluster (~8.7 s of 36 s of bridge
  self-time). `RAVA_STREAM_MAP_STAT` already answers the sizing question: **160,000 map calls over
  629,717 elements = 3.9 elements per call**, almost all n=2..8. So the cost is per-STREAM
  overhead and the lambda apply, not per-element work — which is why the two mechanism rewrites
  already tried there (lazy streams, fused streams) both measured neutral. **Start that arc at the
  apply path, with a census, not at the stream machinery.**

**There is still no single lever to a 4× boot** — the largest named leaf in the whole profile is
8.4% of wall and 39.3% of `JIT_EXEC` has no attribution at all. The two arcs above are worth
roughly 15-25% each, they compound, and each is measurable on its own gate. That is the program.
