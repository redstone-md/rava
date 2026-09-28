# Interpreter parity with `java -Xint` — where the gap is, and what is left

Goal: rava's interpreter, with no JIT, faster than HotSpot's interpreter with no JIT.
Everything below is measured on this machine, P-cores, `High` priority, paired A/B
(`bench/interp_ab.ps1`, which refuses to run while cargo/rava is alive).

## The two benchmarks that split the question

`bench/OpBench.java` is `bench/CallBench2.java` with `s = addS(s,1)` replaced by `s = s+1`.
They differ by exactly one interpreted call, so the pair separates per-opcode cost from
call-protocol cost.

| | rava JIT=0, arc start | rava JIT=0, now | `java -Xint` | gap now |
|---|---:|---:|---:|---:|
| OpBench (≈9 opcodes/iter) | 48.5 ns | **28.4 ns** | 4.72 ns | **6.0×** |
| CallBench2 (same + one call) | 155.4 ns | **146 ns** | 14.0 ns | **10.4×** |

## THE ARC IS CLOSED ON ARITHMETIC (2026-08-13)

Three bounded measurements, each of a different hypothesis, and none of them big enough:

| what was priced | how | cost |
|---|---|---:|
| value traffic | `align(64)` — same power of two, double the bytes | **5.5%** (0/7, DISJOINT) |
| loop-head bookkeeping | bounds check AND the whole safepoint budget removed (unsound) | **4.5%** (7/7, DISJOINT) — an UPPER bound |
| operand decoding | `java -Xint` does the SAME raw decode and is 6× faster | **~0** |

Three levers, each under 6%, do not add up to 6×.

**What is left is the dispatch architecture.** HotSpot's interpreter is hand-written machine-code
templates with the top of the operand stack in a REGISTER and about five instructions per
bytecode. rava's is a Rust `match` in which every push and pop round-trips 32 bytes through
memory and every opcode ends at one shared indirect jump. 28.4 ns for ~9 opcodes is ≈13 cycles an
opcode against HotSpot's ≈2.

The only path that closes it is a template interpreter — machine code per opcode with a
register-resident top-of-stack. That is months-class, it overlaps what the Cranelift tier already
does, and it is **gated on the 16-byte slot** (a 32-byte TOS cannot live in a register), which
section A prices at 5-10% for ~500 sites. Both halves have to be worth it together or neither is.

A pre-decode / quickening arc was designed in full and refuted before a line was written; see
ledger row 222 for the argument and for the two implementation hazards its first step carried.

At the start of the arc both ratios were ~10-11×. **The gap was never concentrated in the
call** — that is what the pair proved, after four shipped steps had gone into the call path
on the opposite assumption.

## What has been done (ledger rows 211-216)

Dispatch loop: the arms that need only the frame moved to `opcodes::hot::run_hot`; the cold
diagnostics inside `#[inline(always)]` frame primitives were outlined (that one was −12.2%);
the stack cursor lives in registers; the back edge stays inside the hot loop; `Value` got a
32-byte slot (`#[repr(align(32))]`, −14.7% — the alignment, not the size).

Call path: resolved call sites, a lock-free site table, bitwise argument moves, and the borrow
flags off the two per-call thread-local caches.

## What is left, with the evidence for each

### A. `Value` at 16 bytes — the biggest measured lever, and the riskiest

24 → 32 bytes measured **−14.7%** purely from becoming a power of two. 32 → 16 keeps that and
halves the traffic, so it should be worth at least as much again.

It needs `Str` to stop being a fat pointer, which is ~500 construction and match sites. **The
risk is not the churn, it is the indirection**: `Arc<str>` → `Arc<String>` puts a second
pointer chase on every string read, and the boot is string-dense (key interning, `Identifier`
rendering, codec decode). A microbenchmark win here can be a boot regression, so this must be
measured on the BOOT, not only on OpBench, before it ships.

Cheaper variant worth pricing first: intern guest strings into a table and make `Str` a `u32`
id. That removes the indirection question but adds an interner lookup on creation.

### B. The flat frame machine — the only structural answer for calls

An interpreted call is still a Rust recursion through `Vm::invoke_idx`. The profile of a call
after all of the above:

    invoke_idx 23.2% · run_hot 18.8% · run 15.5% · invokestatic 11.4%
    call_get 6.3% · frame-buffer pool 8.4% · drop_in_place<Frame> 5.0% · arg_layout memo 2.1%

No row is worth more than a few percent, which is the signature of a structural cost rather
than a hot spot. **Six of the last eight point fixes on this path measured zero-or-worse and
were reverted** — including two "obvious" atomic removals that both came back slower.

The shape that removes the whole block: an explicit per-thread frame stack, where an
interpreted→interpreted call pushes a frame and continues the dispatch loop instead of
recursing, and a callee's locals overlap the caller's operand stack so arguments are never
copied. `Vm::drive_stack` IS that machine — it exists for checkpoint/restore and green threads.

**Measured (ledger row 218).** As written it was 510 ns/call against recursion's 146. Three
incidental costs accounted for most of it: an `env::var` read inside the driver loop, a
by-NAME method lookup on every return to a frame, and two fresh `Vec`s per call in
`build_frame`. Fixed, it runs at **209 ns/call** — and the residue is structural: a
trampolined call pays TWO `run()` prologues (entering the callee, re-entering the caller on
return) where recursion pays one. The trampoline never enters `invoke_idx` at all, so those
209 ns are a call without the 23% the profile bills there, plus two prologues.

**So the next step is to fold the frame stack INTO `run`**: one prologue per outermost entry
instead of one per frame. That is the change that makes this machine faster than recursion
rather than close to it.

⚠ One caveat that decides where it can ship: the invoke opcodes stage the callee BEFORE the
compiled-tier checks, so the trampoline bypasses cnp/aot/Cranelift. As it stands it is a
machine for the JIT-off goal; making it the boot's machine additionally requires moving the
tier dispatch into the driver.

Prerequisites, all of which the current design already half-satisfies:
- GC roots: one contiguous stack is easier to scan than today's chain of per-frame `Vec`s, but
  the frame slab must have stable addresses (a growable `Vec<Frame>` would move them).
- Exception handling: `find_handler` must walk the explicit stack rather than unwinding Rust.
- Re-entrancy: natives that call back into the VM start a nested region; that already works.

### C. What is NOT worth doing

- More point fixes on `invoke_idx`. The eight-attempt record is the argument.
- Collapsing disarmed diagnostics. The five M3 probe guards were collapsed into one armed
  check and measured **neutral** — the branches predict, and the unwind funclets never run.
- Reading anything into gate wall times. A change once appeared to make the ring gates 1.8×
  faster; rebuilding HEAD without it reproduced the same numbers. The gate HASHES are the
  instrument; the milliseconds are not.

## Instruments built for this arc

- `bench/OpBench.java` — the call-free twin of CallBench2.
- `bench/interp_ab.ps1` — paired A/B over two binaries, refuses a busy machine.
- `bench/interp_pc.ps1` — PC profile of the microbenchmarks (every other profiler here points
  at the boot). Needs `release-diag`; `release` is stripped.
- `RAVA_PC_OFFSETS` — buckets PC samples by offset WITHIN a symbol, so a function that is one
  big loop can be read against `dumpbin /disasm`. This is what found the 24-byte slot, the
  in-memory countdown, and the `core::fmt` copies inside `Frame::pop`.
- `bench/wt_boot_ab.ps1` — two-binary boot A/B reporting wall, Done and peak RSS.

## D. The template interpreter — and the machinery for it is already in the tree

Ledger row 222 closed this arc with "only a template interpreter closes it, and that is
months-class". That was true about a template interpreter written from scratch. It is not true
here, and the reason is `crates/rava-interp/src/cnp/`:

> "Each opcode is emitted as INLINE x86-64 (true copy-and-patch — no per-op call/ret): the
> operand stack lives in registers (rsi = stack base, rbx = sp index, rdi = locals base) and each
> op is a few instructions operating on `[rsi + rbx*8 + disp]`."

That is a template interpreter's code generator. It already exists, it already keeps the operand
stack and locals in registers, and — this is the part the measurements said was needed — **it
already works on 8-byte cells rather than on the 32-byte `Value`**. `cnp/asm.rs` is 403 lines and
covers 37 opcode arms.

**What it is not**: a per-METHOD compiler is what `cnp` is used as today, which is a JIT tier. A
template interpreter is the same emitters arranged differently — one stub per OPCODE, generated
once at startup, workload-independent, each stub ending in `load next opcode; jump table[op]`.
That distinction is exactly the one HotSpot draws between `-Xint` (its template interpreter, still
machine code) and C1/C2, so a template interpreter is legitimate under "both without a JIT".

**The first measurable step, and it is not a rewrite**: emit the stub table for the opcodes
`cnp/asm.rs` already covers, run OpBench's loop through it, and compare against the Rust loop's
28.4 ns/iter. Everything not covered bails to the existing `run_hot` at a frame boundary, so a
partial table is always correct. The kill line should be decided before the run: this is worth
building out only if the covered subset lands near HotSpot's ~4.7 ns/iter — a 2× improvement on a
subset that cannot cover the boot is not worth the second machine.

**What it does not fix**: the CALL. At 146 ns against `-Xint`'s 14 ns, calls are the larger gap,
and a template interpreter changes nothing about frame setup, argument marshalling or the
`invoke_idx` path. Section B is still the answer there, and it is still refused as designed.

## E. The host runtime, which is where the boot actually is (2026-08-14)

Everything above prices the INTERPRETER — the dispatch loop and the call. The boot is not
mostly there. `RAVA_PROFILE_PC` over a real boot puts **92.6% of live samples in host code**
and only 3-16% inside compiled guest code (ledger, `rava-host-bound-reframe`). So the engine's
wall is dominated by the RUNTIME: natives, dispatch bookkeeping, the heap and the key/element
derivations around them.

Today produced the sharpest single measurement of that surface in the tree, and it came from a
regression rather than from a search:

| what | cost |
|---|---:|
| `immutable::kind_of` reading one `$immutable` field BY NAME | **wall 57.7 → 68.1 s, Done 19.8 → 22.9** (n=3, DISJOINT) |
| the same read by cached field id | gave all of it back |

The field is carried by NO collection in the boot. The cost was entirely the MISS: a name no
object has ever carried is not interned, `field_id_existing` returned `None`, and a `None` was
never cached — so every probe re-took the interner's `RwLock` read guard and re-hashed the name.

That shape is everywhere. There are ~375 by-name `$field` reads in the tree, and the ones that
matter are the TESTS — `$mapview` on a collection that is not a view, `$str` on a non-String,
`$map` on a non-map. Three of them were fixed today by hand (`map::key_str`'s five,
`immutable::kind_of`, and `map::hashing` was born by-id), and then the general fix landed: the L1
now caches the miss too, stamped with a `FIELD_EPOCH` that is bumped once per newly interned name.

**The ranked next steps on this surface**, all of the same character — remove WORK from a path the
runtime takes on every call, and measure each one alone:

1. `Heap::get_field` scans `o.fields.ids()` LINEARLY for the id. A per-(class, id) slot memo makes
   it O(1); `layout_slot` already computes exactly this for the pinned-mirror path.
2. `class_of(h).to_string()` allocates a `String` per call, on paths that only compare it —
   `value_class_name` (reached per element by `ekey_mut`), `key_render`'s `actual`, `casts.rs`'s
   type-name renderer. `keyroot.rs` already records this exact finding for its own site.
3. The element-key derivation itself: `ekey_mut` builds a `String` per element per scan, where the
   comparison it feeds only needs equality. The scalar arms of `ekey_eq_mut` already avoid it; the
   reference arms do not.

⚠**And the discipline the same day taught**: three changes in this session each looked like a pure
correctness addition and each cost double-digit percentages of wall, because a probe that MISSES is
not free. Land one, measure it paired, then land the next.

## F. How to measure anything here — see `docs/how-to-measure.md` (2026-08-15)

Sections A-E all price levers. This one is about the scale on the wall, and it
supersedes the instinct every section above was written with.

**Eight hypotheses were closed as "zero" in one session by an instrument that
could not have seen any of them**, and re-measuring one of the eight on fixed
work found **−19.9%**. The paired boot A/B resolves about ±4% on wall and 7-15%
on CPU; a change confined to compiled code is worth `0.197 × its gate effect` of
a boot, so even a twenty-percent win inside the compiled tier is ~4% of wall.

Price on a gate — fixed work, paired, alternating, hash-checked every run,
comparing the gate's OWN timer and only within one batch — then convert with the
boot's census (`225 ns × 148,869,385 calls`), and take the sum back to the boot
only once it clears ~5%.

That document also records the seven rules each of which was paid for with a
wrong conclusion in this session, including the two that matter most: only
within-batch pairs are comparable, and a change that is not measurably positive
does not get to add a soundness obligation.

