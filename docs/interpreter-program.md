# The interpreter programme — sized, 2026-08-21

> ⚠⚠**READ ROW 355 FIRST. THIS PROGRAMME IS NOT WHERE THE 30× IS.**
>
> On IDENTICAL work (`ChunkFill 1 16`, same hash `f55ae4b5e17b36c1`) rava's interpreter is
> **1.97×** `java -Xint`, not 30×. The boot's gap is **ONE WINDOW**: `Environment:` →
> `Loaded 1470 recipes` is **3.3×** (23 s vs 7 s, identical recipe and advancement counts), while
> `Preparing level` → `Done` is **~626×** (~1,251 s vs 2 s) — and rava never prints
> `Loading 0 persistent chunks`, `Preparing spawn area` or `Time elapsed`, the three lines the JVM
> emits two seconds after `Preparing level`. **The stall is `ServerLevel` / worldgen-pipeline
> construction.**
>
> Everything below is sound, gate-verified and worth having — it bought −10.9% and −3.19% — but it
> optimises a **2×** gap. **Fix the window first.**


Everything here is a measurement from this tree, with the ledger row that produced it. No item is
included without a number, and the refused items are kept with theirs.

## The headline number (row 353)

**6,647,355,906 interpreted opcodes in 144.9 s = 45.9 M opcodes/second** — about 22 ns, roughly 70
cycles, per bytecode. (`run_hot` executes 5,360,840,197; each of the 1,286,515,709 exits is one
cold opcode in `run`. Count and time come from different runs because the census costs 2.6× —
`SETUP_MS` 96,393 → 257,388 — and the count is stable to five significant figures.)

A template interpreter of the kind `java -Xint` runs is 100-400 M opcodes/s, so we are **3-8×
off the per-opcode pace**. ⚠**That does not reconcile with the 29.4× end-to-end gap**, so either
we execute far more opcodes for the same guest work or a large share of the boot is not
interpretation — **the next question, answerable by counting opcodes on a `-Xint` boot against
ours.**

⇒ At 45.9 M/s a 3% step is 1.4 M opcodes/s. The steps below are real; the number they move must
move by 3-8×, and **a stack slot here is a 32-byte `Value` against the JVM's 8**.

## Why the interpreter

| | compilers OFF | with each engine's compiler | its compiler buys |
|---|---|---|---|
| rava | **1,359.3 s** | 53.5 s | **26×** |
| `java -Xint` / JVM | **46.19 s** | ~6.1 s | **7.6×** |
| ratio | **29.4×** | 8.8× | |

*(row 337, re-measured 341)*

**Our JIT is doing more lifting than HotSpot's and we still land at 8.8×.** The interpreter is the
weak member, and it is the scenario the project is judged on ("when the JVM's JIT is off and ours
is off").

⛔**Do not decompose this gap by phase.** Row 336 read 2.5× / 2.9× / 651× across
`Environment:` / `Preparing level` / `Done` and row 337 withdrew it: row 151 had already
established that the two `Done` windows do not bracket the same operations (the JVM prints
`Preparing spawn area: 100%`, rava never does). **Prove the delimiters bracket the same work before
splitting a cross-engine gap.**

## The instrument (row 339)

`bench\interp_gate.ps1` — `ChunkFill 1 16`, `RAVA_JIT=0`, big nursery.
**`f55ae4b5e17b36c1` interpreted AND compiled**, so it is a CROSS-TIER correctness gate, not a
self-check. ~50 s per run against a 24-minute boot, and its profile reproduces the boot's.

⚠It needs `RAVA_NURSERY_KB=1048576` because **every gate SEGFAULTS under `RAVA_JIT=0`** at the
first collection (`0xC0000005`). That defect is open — see below.

## Where the time goes (row 344, 5,155 samples)

| | share |
|---|---|
| `run_hot` + `run` (the two dispatch loops) | 32.2% |
| **call machinery** | **39.3%** |
| `tlcache` (`call_get` 3.7 · `field_get` 2.3 · `obj_inline_get` 2.0) | 8.0% |

Call machinery: `invoke_idx` 11.7 · `expand_locals_into` 7.1 · `invokevirtual` 4.8 ·
`invokestatic` 4.3 · `pool_take` 2.9 · `drop<Frame>` 2.2 · `pool_give` 1.6 · `arg_layout` 1.0.

⛔**It is not made of removable fat.** `expand_locals_into` (memoised layout, exact reserve,
drain-to-move), `pool_take` (pooled, gated censuses) and `run_hot` (split out of a 57 KB `run`,
frame destructured into disjoint borrows, operand stack on a raw pointer) are each already the
product of a pass. The tier checks are all behind `jit_enabled`; the codec interceptions are
behind a name compare, which Rust resolves by length first.

## ⭐ THE LARGEST ITEM — the hot-loop re-entry (row 348)

`run_hot` returns to `run` for every opcode it does not own, **which includes every field access
and every call**, and each re-entry re-destructures the frame and reloads the operand cursor.

`RAVA_HOT_CENSUS`: **`entries = 1,286,515,702`, `opcodes = 5,360,840,230` ⇒ 4.17 opcodes per
entry.** The split's setup is amortised over four opcodes. At ~5 ns per entry that is **~6.4 s of a
~50 s gate ≈ 13%**, and unlike every other item here it is **above the gate's resolution**.

⚠**The fix is constrained by the header that justified the split**: moving cold opcodes INTO
`run_hot` adds exactly the register pressure it exists to relieve. The change is *stop RETURNING
for a cold opcode* — sync the cursor, call an out-of-line `#[cold]` handler, reload, continue.

⚠⚠**AND MOST EXITS CANNOT BE REMOVED AT ALL (row 349).** Which opcode ends a run, over
1,286,515,599 exits:

| opcode | share | addressable? |
|---|---|---|
| `getfield` | **23.8%** | ✅ |
| `?return` (0xac/0xaf/0xb0/0xad) | **≈26%** | ⛔ the method ends |
| calls (0xb6-0xba) | **32.7%** | ⛔ the callee needs its own frame |
| `ldc` | 6.4% | ✅ |
| `getstatic` | 3.1% | ✅ |
| `putfield` | 1.4% | ✅ |

⇒ **~59% of exits are unavoidable; ~35% are addressable ⇒ the item is worth ≈ 4.5%, not 13%** —
entries 1.29 B → ~840 M, ratio 4.17 → 6.4. **Still above the gate's ±1.8%, so it is the one
interpreter item that can be both built and measured.**

⇒ ⭐**`getfield` alone is 23.8% of exits ≈ 3%** — and it is the SAME `getfield` whose frame
coupling blocks the getter inline below. **Three levers, one refactor: make the opcode
implementations callable without `&mut Frame`** (only ten functions take it). Its value is their
sum.

## The smaller levers, and their sizes (row 345)

`RAVA_SHAPE_CENSUS` over **354,094,225 interpreted calls**, by callee body shape:

| shape | calls | share |
|---|---|---|
| GETTER `aload_0; getfield; ?return` | 15,235,298 | 4.3% |
| CONST `<const>; ?return` | 15,717,978 | 4.4% |
| SETTER | 2,089 | 0.0% |
| **SMALL (< 16 bytes)** | **124,462,158** | **35.1%** |
| OTHER | 198,676,702 | 56.1% |

⛔**REFUSED — inline the trivial accessor.** 4.3% of calls × the frame's share of 39.3% ≈ **1.7%**
of interpreter time, for a refactor of `getfield`, the highest-traffic opcode in the engine. All
three trivial shapes together are 8.7% ⇒ ~3.4%. Refused on its own number, before the edit.

✅**THE ITEM — a general small-body path**, and ⚠**its size was corrected by measurement before
anything was built on it (row 346).** One interpreted call in three has a body under sixteen bytes
and pays a full frame for it.

**What a frame actually costs**, split by making `Frame::new` `#[inline(never)]` and re-profiling:

| | share of interpreter time |
|---|---|
| `Frame::new` — construction | **0.34-0.60%** |
| `drop_in_place<Frame>` — teardown | 2.4% |
| `pool_take` + `pool_give` — buffer traffic | 5.0% |

⭐**And that 5.0% is FREQUENCY, not work** (row 347): `RAVA_POOL_CENSUS` reads **907,019,939 checkouts clearing 1.83 slots each** — against 354,094,225 interpreted calls, i.e. **2.6 pooled buffers per call**. Clearing under two `Value`s is nothing; the cost is the checkout itself (TLS access, deref, pop, branch). ⇒ **the item is FEWER CHECKOUTS — getting 2.6 to 1 is worth ~3% of interpreter time on its own.**

| **total frame cost** | **~7.9%** |

⇒ **Eliminating the frame for 35.1% of calls is worth ≈ 2.8%**, not the ~5% first estimated, and
**construction is not the expensive part — teardown and pool traffic are.**

⇒ The larger number needs `invoke_idx`'s **11.8%** skipped as well, and that 11.8% is NOT frame
building (measured above): it is resolution, the interception chain, the tier checks and result
handling. Bypassing it for a small body means reproducing the resolution its correctness lives in.
**So the honest bracket for this item is ~2.8% (frame only) to ~13% (frame + dispatch), and the
upper end is a correctness-sensitive rewrite rather than a fast path.**

⚠**The `Arc` clone cannot simply go** (row 344): `code: &Code` is borrowed OUT OF `class`, so the
`Arc` cannot be moved into the frame while that reference lives. Removing it means making the
frame's `code` a raw pointer — which `Frame` already stores as `*const Code`.

## Open defect, with a one-variable repro (rows 339-343)

Every gate segfaults under `RAVA_JIT=0` at the FIRST collection. `RAVA_NURSERY_KB=1048576` avoids
it by preventing collection at all. **Seven instances of one cause are fixed** (rows 340-342): a
native holding guest `Value`s in a Rust local across `heap.alloc` / `put_field` / `collection` —
invisible to the root walk. The hot one bought **−10.9%** on the gate. A tree-wide scan for that
shape now finds none, **and the crash survives**, so the remaining cause is a different shape.

⚠`RAVA_RECEIVER_GUARD`'s `HELD-BY <none live>` is NOT evidence of an unrooted value (row 343): it
reports our own natives' declared-vs-actual type substitution, and searches a root set that does
not cover the objects. **The next attempt needs a probe that reports the root set's own coverage.**
