# next-gen-gc — Context Checkpoint (session boundary, 2026-06-20)

Start-context for the next clean-window R&D session. Full history:
`rnd-logs/rava-endgame-migration.md` (Phases 1–9) and `rnd-logs/berserker.md` (benches).

Branch: `feat/os-threads` (= "main"). Default build is byte-identical to
pre-endgame (all arena code is `#[cfg(feature = "next-gen-gc")]`).

---

## ★★★★ SPEED SHIELD + MULTI-CORE — both phases COMPLETE (2026-06-20) ★★★★
`feat/os-threads` (= main) @ `d8195ae`, linear, **pushed to origin (local == origin)**. Default
build **byte-identical** (every change `#[cfg(feature="next-gen-gc")]`). Tests: **default 72 / flag
110**, all microbenches **CORRECT** (SUM byte-exact vs JVM).
- **Phase of Raw Speed (Speed Shield):** every microbench within a small constant of the JVM — the
  per-op callback war is WON (StringOps 272x → ~1.4x, the last residual; table below).
- **Phase of Multi-Core (RavaPool + JIT forker):** a data-parallel Java loop auto-parallelises across
  all cores from bytecode, GIL-free — **6.2x in-VM, 5.5x faster than the JVM on 16 cores** (see the
  Multi-Core Phase section). Opt-in via `RAVA_JIT_PARALLEL`.

**The compiled hot path is now callback-free for the common cases.** Isolated `RAVA_JIT=1` +
`--features next-gen-gc`, recursive (no trampoline):

| bench | before | after | how |
|---|---|---|---|
| **Allocs** | 496x | **~1.6–1.9x** | inline TLAB bump (`new`→0 cb) + trivial constructor inliner (`<init>`→0 cb) |
| **Statics** | 337x | **~0ns** | static-LICM (getstatic hoisted) **+ loop-folding** (counted invariant-accumulation loop → closed form `S * trip`, no loop — faster than JVM) |
| **Dispatch** | 140x | **~1.9x** | 4-way PIC + **native idiv/irem** + **flat arena-Obj arrays** (52M callbacks → 0) |
| **Collections** | 36x | **~9.2x** | **collection intrinsics**: Integer valueOf/intValue box/unbox + HashMap.put direct (invoke 2.6M→1.2M) |
| **StringOps** | **272x** | **~1.4x (parity)** | **ArenaString** + **ldc-String LICM** (compile-time intern → baked iconst) + **native startsWith** + **inline itoa concat** (native digit loop + TLAB bump, no `rava_indy`) — callbacks 7.8M→~0 |
| Fields | 994x | ~3.5x | inline arena field ops (every type) |
| DispatchMono | 3341x | ~6.6x | final-class virtual inline |
| ArithLoop | — | ~1.2x | native Cranelift arithmetic |

Per-object callbacks on allocation + construction + dispatch are **gone**; the residual ratios are
now structural (GC collects, loop-folding, array access), not callback overhead.

### ★ EXPERIMENTAL CONCLUSIONS (2026-06-20) — the last two walls, proven by experiment not theory
The remaining benches (Collections ~36x, StringOps ~268x) are **callback/dispatch-bound**, not
alloc/bootstrap-bound — established by building the requested optimization and MEASURING zero/negative
movement:
1. **Collections needs NATIVE JIT INTRINSICS for HashMap/ArrayList.** Landed the typed `MapKey`
   (`5951882`) — `Value::Int` keys index alloc-free (no `format!`). Collections moved **0** (~36x→36x):
   the String-key alloc was NOT the cost. Each `m.put`/`l.add` is an invokevirtual → `rava_invoke` →
   native collection dispatch (handle-backed; the arena dispatch IC doesn't cover them). Single-digit
   requires inlining the collection ops into compiled code to kill the per-op callback boundary.
2. **StringOps needs ARENA STRINGS (the `0b11` ArenaString rewrite).** Built JIT `invokedynamic`
   (concat-callback) + ldc-String so `hot` COMPILES — measured **268x → 308x WORSE** (reverted). A
   callback-dominated loop regresses when compiled: the compiled↔callback boundary (spill/reload +
   synthetic frame) costs more than the interpreted callback. The only path to single-digit is making
   the string ops (concat/hashCode/length/charAt) NATIVE machine code — i.e. `Value::Str` → a
   low-address arena string struct (`[p+24]`=len, `[p+28]`=byte ptr), the string-subsystem rewrite that
   frees `0b11` from `arena_box`. Compiling the callback loop is a proven dead end.

### ★ STAGE 4 LANDED (2026-06-20) — Dispatch + Collections crushed
- **Lever A — native idiv/irem** (`44e5f14`): inline `sdiv`/`srem` + cold divisor==0 (callback) +
  INT_MIN/-1 wrapping special-case. Killed Dispatch's 26M `i % len` divrem callbacks.
- **Lever B — flat arena-Obj arrays** (`44e5f14`): `encode_ref_cell` stores `Value::Obj` as its raw
  `< 2^40` pointer (hi < 256, disjoint from tags) instead of boxing into `array_box`/ABOX; `native_
  aaload` + `decode_ref_cell` read it inline. Killed the 26M `shapes[i]` aaload callbacks. → **Dispatch
  8.6x → 1.9x.**
- **Lever C — collection intrinsics** (`a17c27b`): `Integer.valueOf/intValue` → thin box/unbox
  externs (zero callback — the ~4 autoboxes/iter were the cost); `HashMap.put` → `rava_map_put`
  direct (skips the synthetic frame). → **Collections 36x → 9.2x (single digit).** Residual = the
  remaining native ops (ArrayList.add/size, getOrDefault, the map-body) — further direct externs push
  lower; single-digit met.
- All gated → default byte-identical; tests default 72 / flag 96; full bench all CORRECT.

### ★ ARENA-STRING SUBSYSTEM LANDED (2026-06-20) — StringOps 270x → ~140x (`88f3aac`)
A Java String can live on the arena as a `Value::Obj` of a dedicated **ref-mask-0** string class —
so the Cheney collector copies it (`obj_size(nf)` bytes) and never traces the inline byte cells. **NO
GC-core change, NO new `Value` variant.** Layout: header(24) + `[24]`byte_len `[28]`char_len `[32]`hash
`[40..]`inline UTF-8; `nf = 2 + ceil(byte_len/8)`. Proven by `arena_string_survives_collection`
(bytes + hash survive a move, incl. multi-byte). The `str_of`/`value_to_string`/`display`/`key_str`
DUALS protect all ~430 `Value::Str` sites (an ArenaString works + keys identically everywhere). JIT:
`invokedynamic` concat + ldc-String compile + PROMOTE their result to an ArenaString; `String.length()`/
`hashCode()` are intrinsics (arena receiver → native `load.i32` of cached char_len/hash, else thin
callback). **`StringOps.hot` now COMPILES (was a hard bail); ~270x → ~140x, SUM byte-exact.**
- **Residual to single-digit:** the per-iter concat is a CALLBACK (`rava_indy` = itoa + assembly +
  alloc, `invoke≈5M`) + the loop-invariant `"item0"` ldc (`other≈2.6M`) + startsWith. Single-digit
  needs **INLINE concat codegen** (native itoa digit-loop + inline TLAB byte-bump), ldc-String hoisting
  (it's loop-invariant), and a startsWith intrinsic — the next lever.

### ★ STRINGOPS ANNIHILATED (2026-06-20) — 272x → ~1.4x parity (`7da9fea`, `61d0fc8`)
All three residual StringOps callbacks killed; the bench now runs at JVM parity (faster than a hot
Java baseline). Three levers, every change gated → default byte-identical, SUM byte-exact throughout:
1. **ldc-String LICM (`7da9fea`)** — a String constant is loop-invariant + immutable, so it is
   interned ONCE at COMPILE time into a permanent **low-arena (<2^40) bump pool** and its stable
   address baked as an `iconst`. No per-iter `rava_ldc` callback, no nursery churn, auto-hoisted. The
   pool is non-nursery → the collector leaves baked pointers untouched (`copy_obj` returns non-nursery
   ptrs as-is) and never traces the ref-mask-0 bytes; layout byte-identical to `tlab_alloc_string`.
   → `rava_ldc` 2.6M→0, ARENA_ALLOCS 5.2M→2.6M. **272x → 68.8x.**
2. **`String.startsWith` intrinsic (`7da9fea`)** — both-arena receiver/prefix compare bytes inline
   (load byte-lengths, bail if `recv<prefix`, byte-equality loop); mixed operands → thin
   `rava_str_startswith` callback. Kills the per-iter dispatch-miss. **68.8x → 9.2x (single-digit).**
3. **Inline itoa concat (`61d0fc8`)** — a `makeConcat[WithConstants]` whose dynamic args are all `int`
   and recipe ASCII is resolved at compile time to a `ConcatShape` (baked literal runs + per-arg itoa
   positions). Compiled code shreds each int via a native divide-by-10 digit loop (sign-aware,
   INT_MIN-safe) into a stack scratch, computes total length + object size, bumps the TLAB cursor ONCE
   to allocate the ArenaString, copies literals + digits, and folds the Java hash inline — all in
   Cranelift IR, **zero `rava_indy` callbacks**. Overflow → callback fallback (collect + retry). Layout
   byte-identical to `tlab_alloc_string`. **9.2x → ~1.4x.** Validated byte-exact incl. negatives, zero,
   3-arg concats, big multi-digit values, and a forced-overflow 64KB nursery (slow path).

### ★★★ MULTI-CORE PHASE — GIL-FREE JIT AUTO-PARALLELISM (2026-06-20) ★★★
The Speed Shield won the single-core callback war; this phase makes a data-parallel Java loop utilise
ALL cores straight from bytecode, GIL-free. Merged to `feat/os-threads` @ `d8195ae` (= `feat/jit-speed-abi`,
linear). All gated `next-gen-gc`; the fork itself is opt-in (`RAVA_JIT_PARALLEL`). default 72 / flag 110.

- **Kit 1 — work-stealing pool (`67ebe15`).** `parallel_pool::RavaPool` on crossbeam-deque (global
  `Injector` + per-worker `Worker`/`Stealer`), layered on the `crate::parallel` region/safepoint
  primitives. `parallel_for` fans chunks across lanes with the `gilfree` flag set + isolated thread-local
  TLABs; `abort_region` → serial-GIL fallback on a worker overflow.
- **Kit 2 — TLAB memory bridge (`b1bc28b`).** `nextgen` `tlab_try_alloc`/`tlab_alloc_string`: a `gilfree()`
  worker that overflows raises `parallel::request_abort()` + returns None (NEVER collects GIL-free) → the
  region aborts → serial retry. Coordinator pre-sizes a clean fixed nursery per worker under the GIL
  (`prewarm_for_parallel` + `worker_tlab_init`). Proven: isolated concurrent ArenaString alloc (distinct
  addresses) + overflow→`RegionAbort`→clean serial fallback.
- **Kit 3 — JIT forker + reduction (`a113769`, `d8195ae`).** Three pieces:
  * **brain** `jit::analyze_data_parallel_loop` — detects a counted associative-accumulator loop
    (const-step counter, int/long acc, `+`/`*`/`^` fold, trip ≥ 10k const-or-runtime-guarded, body =
    PURE counter-dependent int arithmetic) and **rejects any shared write / call / alloc / branch**.
  * **muscle** `RavaPool::parallel_reduce` — folds `[0,trip)` across the pool via the body ABI
    `extern "C" fn(lo,hi,*mut ok)->partial`, partials combine by `ReduceOp{Add,Mul,Xor}` (associative →
    order-independent); `*ok=0` (TLAB overflow) → `RegionAbort`.
  * **codegen** `build_reduce_body_fn` + `emit_body_expr` compile the body to a SECOND Cranelift fn;
    `build_parallel_body` emits the trip-guard → `rava_parallel_reduce(body_addr,trip,op,*abort)` →
    serial-fallback-loop on abort/sub-threshold, returning `combine(acc_init, result)`.
  * **PROVEN end-to-end** (ParSum, `long acc += (long)(i*31 ^ (i>>3) ^ (i<<7) ^ (i*2))`, 200M trip):
    serial-JIT **192ms → fork 31ms = 6.2x in-VM**, byte-exact vs JVM, **5.5x faster than the JVM** on 16
    cores with **zero GIL contention** in the region (`peak_lanes=16`). ArithLoop correctly does NOT fork
    (`acc ^= acc<<13` is loop-carried → genuinely sequential; the analyzer rejects it).

**Future increments (TЗ for the forker):**
1. **Extend the Purity Gate to TLAB allocations** — admit `new` (0xbb arena-eligible) + `makeConcat`
   (0xba ConcatShape) inside the body so ALLOC-bodies fork. The body fn's inline TLAB bump must, on
   `top+size>limit`, write `*ok=1`-error and early-return (the abort path is already plumbed end-to-end;
   wire the JIT inline-bump overflow / `rava_arena_safepoint` to the `gilfree`-abort branch).
2. **`int`-accumulator support via mod-2³² truncation** — fold the i32 partial sign-extended in i64
   through `parallel_reduce`, then `ireduce` the final result (low 32 bits are exact for `+`/`*`/`^` by
   the mod-2³² ring homomorphism). Lifts the current long-acc-only codegen restriction.
3. **Generalise the loop shape** — step ≠ 1 (runtime `trip = ceil((lim-c0)/step)`), and capture
   loop-invariant parameter locals into the body via a context pointer (today the codegen is counter-only).
4. **Resident pool** — replace the per-region `thread::scope` spawn with parked persistent workers once a
   real workload (parallel worldgen) drives steady-state forking.

### Residual front (next sessions)
0. ~~StringOps → single-digit~~ **DONE** (272x → parity). The ArenaString + LICM + inline-itoa stack is
   the template for any future string-heavy hot path.
0b. ~~JIT auto-parallelism~~ **DONE** (6.2x in-VM, 5.5x > JVM, 16 cores GIL-free) — see the Multi-Core
   Phase above; extension ТЗ recorded there.
1. **Collections deeper** — add `ArrayList.add/size` + `getOrDefault` direct externs (the remaining
   1.2M invokes) for ~5–6x. Note ArrayList.add must NOT apply to Set (dedup) + handle lazy `$elems`.
2. **StringOps (~268x) — interpreter-invoke-bound, NOT box-table.** PROFILED (2 sessions): `hot`
   does NOT compile — `invokedynamic` (`0xba`, `makeConcatWithConstants`) is unsupported in the JIT →
   the whole loop runs interpreted. Per iter = 4 invokes (concat-indy + hashCode + length +
   startsWith); the string ops are already native+fast (`String.hashCode` = native byte-fold,
   `value_to_string(Int)` = native `display()`). LANDED: an **invokedynamic CallSite cache** (`34ad529`,
   `tlcache::indy_*`) — resolve the bootstrap Plan once per site instead of every call (real MC-boot
   value: lambdas/concat); but StringOps barely moves (~274→268x) because the bootstrap resolution was
   a tiny fraction. **Single-digit needs JIT `invokedynamic` support** (so `hot` compiles + concat/
   methods get fast dispatch) — a real feature, own round. The arena-string `0b11` retag is NOT the
   lever (and `0b11` is still taken by `arena_box` — boxed `Str`/`StdOut`/boxed-prim; relocating it is
   a separate string-subsystem rewrite, only worth it AFTER strings are arena objects).
3. **Collections (~36x) — callback/dispatch-bound, PROVEN.** `Integer.valueOf` is already a no-op
   (identity `Value::Int`, zero alloc). LANDED the typed map key (`5951882`, `enum MapKey{Int(i64),
   Str(Box<str>)}`): a `Value::Int` map key now indexes alloc-free (no `format!("i:{n}")`); every
   other key keeps its exact string form (registries byte-identical). Tests default 72 / flag 95
   (+int_map_keys). **But Collections is UNCHANGED (~36x)** — the alloc elimination → 0 bench movement
   PROVES the String key was not the bottleneck. The cost is the per-op **callback**: each `m.put`/
   `l.add` is an invokevirtual → `rava_invoke` → native HashMap/ArrayList dispatch (handle-backed; the
   arena dispatch IC doesn't cover them). Single-digit needs **native JIT intrinsics for the collection
   ops** (eliminate the per-op callback boundary), not key allocation. MapKey kept as a correct, low-
   risk allocation saving + foundation.
4. **StringOps (~268x) — compiling it is a REGRESSION (PROVEN, reverted).** Built JIT support for
   `invokedynamic` (0xba, concat-via-callback) + ldc-of-String so `hot` COMPILES — and it went **268x →
   308x WORSE**. Definitive: StringOps is callback-bound (per iter = concat-indy + ldc-String + 3
   string-method invokes, all callbacks), and the compiled↔callback boundary (spill/reload + synthetic
   frame) costs MORE than the interpreted callback. JIT-compiling a callback-dominated method hurts —
   the `callback_heavy` count heuristic under-weights callback COST. Reverted (kept `hot` interpreted at
   268x). **StringOps single-digit needs the string ops NATIVE in compiled code = arena strings**
   (`Value::Str`→arena struct, the `0b11`-retag string-subsystem rewrite), NOT compiling the callback
   loop. The 0xba/ldc JIT path is real infrastructure but only pays off once strings are native.
3. ~~**Loop-folding**~~ **LANDED** (`feat/jit-speed-abi` @ `8154a54`): a whole-method counted
   invariant-accumulation loop (`acc = C; for i<LIM: acc += INV; return acc`, INV invariant from
   getstatic/int-const/int-arith only) folds to the closed form `C + i2l(S)·max(0, LIM−C0)` (wrapping
   i64, bit-exact) — no loop. `analyze_trivial_loop` is a rigid whole-method matcher (a match ⇒ the
   method computes exactly `acc_init + INV·trip`); `build_folded_body` emits the closed form, fully
   isolated from `build_body`. **Statics 92x → ~0ns** (SUM byte-exact, the 20M loop is one multiply,
   faster than JVM). Tests flag 94. NEXT generalisation: int-acc / step≠1 / `<=`/`>` conditions /
   non-static invariant bodies (getfield-invariant), and a Gauss fold for `acc += i` (arithmetic-series).
4. **Precise GC stack maps** — replace the conservative JIT-buffer root scan (8-aligned + non-null +
   in-nursery) for type-erased local slots; **back-edge STW poll** (deferred until multi-thread STW).
5. **Netty/log4j native blocker** — `unsupported native invokestatic org/apache/log4j/Logger.getLogger`
   via netty at `server.Main`, BEFORE block-state; make unsupported-native dispatch throw a catchable
   `LinkageError` so netty's logger probe falls back, to advance the recursive MC boot.

---

## ★★★ MERGED TO MAIN (2026-06-19) — Phase of Raw Speed landed ★★★
`feat/jit-speed-abi` fast-forwarded into `feat/os-threads` @ `80afc3b` and pushed to origin
(9 commits, linear, no merge commit). Tests: **default 71 / flag 88**, default byte-identical.

**The trampoline is GONE from compiled hot paths.** With `RAVA_STACKFUL` UNSET (recursive) +
`RAVA_JIT=1` + `--features next-gen-gc`, compiled code runs JIT-engaged on the live moving arena:
- **Fields 994x → ~4x JVM**, **DispatchMono 3341x → ~9x**, ArithLoop ~3x — the field-op front is
  all inline machine code (getfield/putfield every type + acmp, **zero callbacks**), proven by the
  low-2-bit ABI on a low-address (<2^40) arena.
- **Recursive JIT survives Cheney GC:** `Allocs` runs through **206 live evacuations mid-compiled-
  code**, forwarding compiled-frame spilled roots, **SUM byte-exact vs JVM, no segfault**
  (`spill_for_gc` → GC-capable callback → `arena_collect_jit` forwards `cells_all` in place →
  `reload_after_gc`).

How we got here (the arc, detailed in §4 + the Task 1/Task 2 sections below):
1. Tracer proved the incremental ABI flip is **unsound on plain heap** (Windows heap ~2.4 TiB aliases
   the high-word tag bits) → **low-address arena (<2^40)** makes `addr>>40==0` / `cell&3` sound.
2. Inline field ops (Task 1) on the sound discriminator → field front fully inline.
3. Native safepoints (Task 2 core): `reload_after_gc` + `arena_collect_jit` + compiled-`new`→arena.

Next targets (full design in the Task 2 section): ~~inline TLAB bump~~ **LANDED**, ~~constructor
inliner~~ **LANDED**, ~~static-LICM~~ **LANDED** (see below), precise GC stack maps, back-edge STW
poll, the **machine-code dispatch IC** (analysed below), and the **Netty/log4j native blocker** to
take the recursive path through a full MC boot.

### ★ STATIC-LICM — LANDED (branch `feat/jit-speed-abi` @ `6861f13`)
getstatic in an **invariant method + loop** populates its static value cells ONCE at entry (a
`getstatic_cached` prologue per slot) and reads them in the body as `readonly` loads → the egraph
CSEs the base and **hoists the loop-invariant statics out** (what C2 does). `has_back_edge` gates it
to looping methods (the prologue is a one-time per-call cost); `Env.static_licm` = invariant + loop +
≥1 cell-backed primitive static. Gated → **default byte-identical**.
- **LIVE (Statics):** SUM byte-exact; hot-loop getstatic callbacks → 0 (only the entry prologue,
  8 statics × 7 compiled calls = 56). **63ms → ~22ms, ~337x → ~92x JVM.** Residual = loop-folding
  (C2 folds `acc += CONST` over 20M iters to a closed form; rava runs the bare loop) — a separate
  lever (induction-variable / loop-accumulation recognition), orthogonal to getstatic.

### ★ DISPATCH IC — LANDED (branch `feat/jit-speed-abi` @ `d2ff434`)
invokevirtual/invokeinterface on an **arena receiver** now runs through a **machine-code 4-way PIC**,
not the `rava_invoke` callback. Per call site a leaked, zeroed `[(class_id i64, fptr i64); 4]` cache
(baked address): read `class_id` from the arena header `[recv+0]`, unrolled-compare, on HIT
`call_indirect` the compiled callee DIRECTLY (no Rust runtime / synthetic frame / HashMap); on miss /
non-arena / empty → `rava_dispatch_miss` resolves + runs + fills a slot. **Why new:** the existing
Rust PIC (`jit_direct_call`) only handles HANDLE receivers (requires a REF_TAG cell), so arena
objects (the Dispatch shapes: double fields → arena) always took the slow general path = the 140x.
**Soundness:** only GC-LEAF callees cached (`is_gc_leaf`: no invoke/alloc/getstatic/instanceof/athrow/
monitor; getfield/putfield/array ops never allocate) with `max_locals ≤ 16` — a leaf never spills, so
the HIT path needs no spill/reload and the non-zeroed buffer is never scanned; buffer-tail garbage a
callee loads into an extra local is overwritten before use (verifier) and never GC'd. Miss path
threads ref operand-stack values through the merge (the inline-allocator SSA-diamond pattern).
No-wide-arg sites only (slot == operand). Gated → **default byte-identical**. Tests default 71 / flag 92.
- **LIVE (Dispatch):** SUM byte-exact; ~26M `area()` invoke callbacks → **3** (warmup misses).
  **~140x → ~8x JVM.** Residual = array access (aaload + arraylength per iter, `shapes[i % len]`),
  NOT dispatch — the next array lever. DispatchMono unchanged (final-class inline ~8x); full bench all
  CORRECT.

### StringOps inline — analysed, NOT built (tag conflict)
The spec'd plan (mark cell tag `0b11` = raw ptr to an arena string struct, `[p+24]`=len `[p+28]`=byte
ptr) **conflicts with the live scheme**: `0b11` is already the BOXED-VALUE tag (`(idx<<2)|0b11` →
`nextgen::arena_box` side table, holds `Str`/`StdOut`/boxed-primitive). Repurposing it needs
relocating boxed values to another mechanism + a string-struct layout + the actual StringOps inlining
(295x) — a real architectural round, not a "prep" constant. Deferred to avoid dead/conflicting code.

### ★ INLINE TLAB BUMP — LANDED (branch `feat/jit-speed-abi` @ `6df5f7d`)
Compiled `new` of an arena-eligible, already-initialised class bump-allocates **inline, zero
callback**: a thread-local `JIT_CURSOR` `[next,end]` (lock-step with `young` via
`sync_young_from_cursor`/`refresh_cursor` in `tlab_try_alloc`+`tlab_collect`) is bumped in machine
code; header+cells init inline; overflow spills → `rava_arena_safepoint` (→`arena_collect_jit`
forwards roots + republishes cursor) → reload → machine back-edge retry (no trampoline). SSA across
the overflow diamond: locals via `def_var`, ref operand-stack values threaded as retry/cont block
params. Compile-time `new_arena` map (`Vm::build_new_arena`: not native-backed, not enum, AND
`initialized`) → `(class_id,nfields)` threaded through `compile_with_static_intrinsics`. All gated;
**default byte-identical**. Tests: default **71**, flag **89** (+`jit_inline_arena_new_fires_no_callback`).
- **LIVE (Allocs, RAVA_JIT=1, recursive):** SUM byte-exact; `new` callback → **honest 0**
  (ARENA-PROBE: 11M measured-loop allocs inline, `ARENA_ALLOCS`=4.5M = the 9 interpreted warmup
  calls only; 142 overflow safepoints). Wall-clock **~263x→~207x JVM (~21% faster)**.
- ~~**Residual:** the per-alloc `Point.<init>` invoke callback~~ **CLOSED — see below.**

### ★★ CONSTRUCTOR INLINER — LANDED (branch `feat/jit-speed-abi` @ `48ae2c4`) ★★
The last per-alloc callback on value-class construction is gone. A **trivial** `<init>` (pure field
setter: `aload_0; invokespecial Object.<init>()V; (aload_0; *load argX; putfield f)*; return`,
`analyze_trivial_ctor`) folds into its `invokespecial` site as **direct field stores** on the
freshly bump-allocated object (`emit_inline_ctor` pops args+receiver → the proven dual-path field
writers: primitives inline raw bits, refs encode→low-2-bit + `rava_card_mark`), **no `rava_invoke`**.
`Vm::build_ctor_inlines` resolves arena-eligible+initialised+trivial sites → a `CtorInline` plan
(arg→slot/id/type), credited as native in `callback_heavy`, threaded through the compile sig. Gated;
**default byte-identical**. Tests default **71** / flag **91** (+analyzer + e2e zero-invoke-callback proof).
- **LIVE (Allocs, RAVA_JIT=1, recursive):** SUM byte-exact; the compiled loop's **11M `Point.<init>`
  invokes drop to 0** (`<init>` seen 15.5M→4.5M = interp warmup only; `invoke` 26.5M→4.5M). Wall-clock
  **9.1s → ~0.07s, ~207x → ~1.9x JVM** — alloc AND construction now pure machine code per object,
  landing near the sandbox ceiling (~1.06x). Full bench: all CORRECT, other benches unchanged.
- **Remaining ~1.9x:** GC (142 collects) + the kept-chain traversal; no per-object callback left.
- **Pre-existing (NOT this change):** `RAVA_NO_ARENA_INLINE=1 RAVA_JIT=1` segfaults on Allocs on the
  PRE-inline baseline binary too (the cold field-inline-off + recursive-arena path) — orthogonal.

---

## 1. Architectural status — what is LIVE under `--features next-gen-gc`

A raw-pointer, moving-GC object model running **alongside** the legacy handle model,
proven semantically identical to the JVM across the entire MC 1.12.2 bootstrap
(joptsimple → log4j → DispenserRegistry → SoundEffects → guava DFU/registry →
block-state registration).

- **Object model:** `Value::Obj(ArenaPtr)` (raw `*mut`, 8-aligned) beside `Value::Ref(u32)`
  (handle). Arena header (HDR=24): `[0]`class u32, `[4]`nfields u32, `[8]`fwd ptr,
  `[16]`identity-hash word, `[24..]`inline i64 cells.
- **TLAB:** thread-local bump nursery (`nextgen.rs`), `RAVA_NURSERY_BYTES` (default 4MB).
- **Cheney moving GC:** young→tenured evacuation + forwarding, live (`tlab_collect`).
  Demonstrated on live McProbe (2KB nursery → safepoint: 7 evacuated, 15 roots forwarded).
- **Card table / remembered set:** active write barrier in `arena_putfield` (old→young).
- **Cross-GC rooting:** `arena_handle_roots` feeds handles held in arena cells into the
  mark-sweep heap GC (else dangling). SHADOW-GC validated **498 roots, mismatch=0** on a live boot.
- **GC-stable identity hash:** lazy xorshift64* stamped into the header (`arena_identity_hash`),
  survives evacuation. Wired into `identity_hash`/`ref_eq`/`ref_equal`.
- **Dual native contract:** native-backed classes (collections/maps, ENUMS, no-bytecode JDK
  classes like `java/io/File`) alloc as **handles**; pure bytecode classes alloc as **arena**.
  Cross-gen arena cells carry tagged handles (low 2 bits: `01`=Ref, `10`=Array, `11`=boxed Str,
  `00`=ArenaPtr/null). Native map key-derivation + `value_to_string` run arena objects' real
  toString/getfield.
- **JIT arena support:** `rava_getfield`/`putfield`/`instanceof`/`acmp` recognise arena
  receivers (currently via the cold `untag-miss` → `unbox_ref` path — see blocker §2).

**Live config that boots MC:** `RAVA_STACKFUL=1 RAVA_JIT=1 RAVA_NURSERY_BYTES=4194304
RAVA_HARD_MB=3584 RAVA_PROC_CAP_MB=4096`. Tests: 82 flag + 37 JIT green.

---

## 2. The performance blocker (physical, measured)

The bootable flag config is **interpreter-speed**, not JIT-speed. Two coupled causes:

### (a) Trampoline runs the interpreter; JIT never engages on the hot loop
Arena allocation needs a **safepoint** to run Cheney GC (move objects, forward roots).
Only the explicit-stack trampoline (`RAVA_STACKFUL=1`, `drive_stack`) can pause at a clean
op boundary (`RunExit::NurseryFull` → `arena_safepoint_gc` → retry). The recursive path
(`invoke`/`jit_run`) holds frames on the Rust stack → cannot forward them → recursive+arena
**segfaults**. So the flag boot MUST use the trampoline.

But the trampoline drives `opcodes::run` (interpreter) frame-by-frame and **does not dispatch
into compiled (Cranelift) code** for the hot loop. Result: every compute-bound microbench
collapses to interpreter+trampoline speed even with `RAVA_JIT=1` (Statics 352x→52991x JVM,
Fields 6.4x→994x, DispatchMono 5.3x→3341x). The arena alloc win is swamped: Allocs 344x→496x
(WORSE). Only Collections improves (46.5x→35.4x). Sandbox proves arena's true ceiling is
**~0.87–1.06x JVM** — entirely masked here.

### (b) JIT cell ABI mismatch → arena field access is a cold callback, not inline
JIT operand cells are **high-word-tagged** (`REF_TAG=1<<40`, `ARRAY_TAG=1<<42`,
`BOX_TAG=1<<41`). A raw 64-bit `ArenaPtr` cannot carry a high-word tag, so `box_ref` boxes
`Value::Obj` into the per-thread box table → a `BOX_TAG` cell. The compiled `getfield`/`putfield`
fast path (`untag_ref`) only decodes flat REF/ARRAY handles → **misses on every arena receiver**
→ falls to the cold `unbox_ref` → `arena_getfield` Rust callback. Correct, but a function call
per field op instead of an inline load. The arena's **low-2-bit** raw-ptr scheme and the JIT's
**high-word** scheme are incompatible — you can't branch-on-low-2-bits in Cranelift IR because
the cell isn't the raw pointer.

---

## 3. Next round — exact technical tasks

### Task 1 — Unify the JIT cell ABI onto 2-bit tags (inline arena field access)
Replace the high-word tag scheme in JIT operand cells with the arena's low-2-bit scheme so an
`ArenaPtr` flows as its **raw pointer** (low bits `00`) and a handle as `(h<<2)|01`. Then emit a
**dual path in Cranelift IR** for `getfield`/`putfield`/`acmp`:
- low 2 bits `00` (ArenaPtr) → inline cell load/store at `[HDR + slot*8]` off the raw pointer,
  **no callback**.
- low 2 bits `01` (handle) → existing indirect heap path.
Touch set (large, default-affecting — do it on a fresh branch, keep default gated/identical):
`pack_local`/`box_ref`/`unbox_ref`/`untag_ref`/`cell_to_root`, the `getfield`/`putfield`/`acmp`/
`aload`/`aastore` IR emitters, GC cell-scan (`frames::cells_all` → `cell_to_root`), arg packing
in `jit_run`. This is the single biggest lever toward the sandbox's 1.06x.

### Task 2 — Native JIT safepoints (delete the trampoline from hot paths)
Let **compiled code** poll a safepoint and run Cheney GC itself, so arena allocation no longer
requires `RAVA_STACKFUL`. Emit a safepoint check at loop back-edges / alloc sites in the JIT;
on nursery-full, spill live SSA refs to the GC root buffer (the spill area already exists,
`jit::spill_for_gc`), run `tlab_collect`, reload forwarded pointers. This lets the flag config
run with the recursive/compiled path (no trampoline overhead) and JIT engaged → the microbenches
stop collapsing to interpreter speed.

### Boot blocker still open (orthogonal to perf)
StateDefinition wall: `NPE: null value in entry ... BlockStone$EnumStoneVariant ... =null` — the
block-state property→value map (`BlockStateContainer` + `java.util.stream` Cartesian product)
puts a null under the flag (default passes). Deep stream/state subsystem.

---

## 4. SESSION FINDING (2026-06-19, branch `feat/jit-speed-abi`) — the incremental tracer is UNSOUND

Attempted the de-risking tracer bullet for Task 1: inline arena field access on ONE op
(`getfield` int const-slot), keeping high-word handles default for every other op, distinguishing
the two cell forms at runtime. **It cannot be done.** Two coupled, *proven* reasons:

1. **No runtime bit-discriminator separates a raw pointer from a high-word handle during
   coexistence.** `obj & 3` is unsound (a high-word handle `h|REF_TAG` has low 2 bits `h & 3`,
   which can be `00`). The fallback `obj >> 40 == 0` is ALSO unsound: a Windows heap pointer is at
   ~2.4 TiB (measured: `vec![0u8;_]` → `0x22a6d574040`, `>> 40 == 2`), i.e. it occupies bits
   40..46 — *the same bits as* `REF/BOX/ARRAY/ABOX_TAG` (1<<40..1<<43). A `debug_assert!(base <
   2^40)` in `Region::new` fired on 9 `nextgen` tests, killing the assumption. (A low-stack buffer
   masked it in a unit test — misleading; always test pointer logic with a *heap* allocation.)
   → A raw pointer and a high-word handle **cannot share one cell namespace.** That is precisely
   why the live code already boxes `Value::Obj` into a `BOX_TAG` cell. The "elegant collapse" is
   real but **must be ATOMIC**: Task 1 has to flip *every* ref cell (handles → `(h<<2)|01`) at
   once, so the discriminator is `cell & 3` (sound regardless of address). It is not tracer-able
   one op at a time. Same blocker hits the GC scan: `cell_to_root` over `cells_all` is type-blind,
   so a raw-`Obj` cell in a ref local is unclassifiable until the whole namespace is low-2-bit.

2. **Task 1's payoff is UNMEASURABLE before Task 2.** Empirically: arena alloc only runs under the
   trampoline (`RAVA_STACKFUL`), and the trampoline drives the interpreter, not compiled code →
   `Fields` ran at interpreter speed, `jit=0`. The recursive/JIT path segfaults on arena (the
   known Task-2 wall — reproduced here, independent of the tracer). So even a *perfect* inline
   getfield never executes until the trampoline is gone. **Strong implication: do Task 2 (native
   safepoints) BEFORE Task 1**, or Task 1 ships blind (unit-tests + interp-path semantics only).

**What landed (sound, kept):** `jit::jit_abi` — the unified low-2-bit primitives `unify_pack` /
`unify_untag` / `unify_cell_to_root` (gated, `+109` lines, default byte-identical by construction:
zero non-gated edits). These are the foundation for the atomic flip. Test
`jit_abi_unified_cells_are_address_independent` proves `cell & 3` works on a real heap (high)
address. Everything else from the tracer attempt was reverted as unsound.

**Decision needed (next session):** (A) atomic ref-ABI flip [big, the real Task 1]; or (B) reorder
to Task 2 first so Task 1 is observable; or (C) constrain arena regions to `< 2^40` via a custom
low-address mapping (VirtualAlloc/mmap base hint) so a high-bit discriminator becomes sound and the
incremental tracer is revived. (A)+(B together) is the leading path.

### RESOLUTION — chose (C): low-address arena, tracer revived (same session)
- **`nextgen::Region` now maps low** (`VirtualAlloc` Windows / `mmap MAP_FIXED_NOREPLACE` Linux /
  leaked-box fallback) at a base `< 2^40`; exits loudly if no low VA. Every `ArenaPtr` is now
  `< 2^40` (test `arena_objects_live_below_2_pow_40`), so `obj >> 40 == 0` is a SOUND ptr/handle
  discriminator (a high-word handle has `hi >= 256`; coexistence with high-word handles preserved).
- **Tracer re-applied on the sound discriminator:** `box_ref`/`pack_local` pass `Value::Obj` as the
  raw pointer; `unbox_ref`/`cell_to_root` decode it (GC sees arena receivers in compiled cells);
  `getfield` int const-slot emits the dual path (inline `[obj+HDR+slot*8]` when `obj>>40==0`, else
  the unchanged handle path). Opt-out `RAVA_NO_ARENA_INLINE`. All gated; **default byte-identical**.
- **Codegen PROVEN:** `jit_inline_arena_getfield_int_fires_no_callback` compiles a real
  `getfield`-int, runs it against a hand-built arena object, gets the field back, and the `getfield`
  callback counter does not move → zero-callback inline read. Tests: default **71**, flag **85**.
- **Still Task-2-gated for LIVE measurement:** `Fields` recursive+JIT+arena still segfaults (`jit=0`,
  reproduced with inline OFF and independent of the ABI) — the documented recursive+arena wall, NOT
  the tracer. So the live `[CB] getfield=0` on a running bench needs Task 2 (native safepoints).
  Branch `feat/jit-speed-abi` @ commits `jit_abi` → low-addr arena → inline getfield.
- **Next:** extend the dual path to `putfield`/`acmp`/`aload` + long/float/double/ref fields; then
  Task 2 (native JIT safepoints) to delete the trampoline and unblock the live measurement.

### Task 1 inline front CLOSED (same arc) — all field ops inline
- **getfield** all types: long/float/double inline-load (raw bits); **ref** re-tags the arena
  low-2-bit cell → JIT high-word operand (`Ref→REF_TAG`, `Array→ARRAY_TAG`, raw `Obj`/null pass
  through, boxed Str → cold callback).
- **putfield** all types: primitives inline-store raw bits; **ref** encodes the JIT cell → low-2-bit,
  inline-stores, then a thin `rava_card_mark(obj,val)` barrier (gated extern + `Env.card_mark`);
  boxed → callback.
- **acmp**: already arena-correct — raw-pointer identity, and a `<2^40` arena ptr never matches the
  `BOX_TAG` high word, so the existing fast path covers it (test-only, no codegen change).
- Shared `emit_is_arena` discriminator. Opt-out `RAVA_NO_ARENA_INLINE`. All gated; **default
  byte-identical** (the `cb` thread-local tally is `cfg(test)` only). Tests: default **71**, flag **88**
  — getfield/putfield (every type) + acmp on arena objects fire **ZERO** runtime callbacks
  (race-free via a per-thread callback counter). Codegen-level proof; live bench still Task-2-gated.
- **Now truly next: Task 2** (native JIT safepoints) — delete the trampoline so recursive+arena+JIT
  runs and the live `[CB] *=0` measurement on `Fields`/MC becomes observable. Remaining inline gaps:
  non-const-slot field ops (`None` slot) still take the handle path (fine — arena classes have const
  slots); `aload`/`aastore`/`instanceof` for arena receivers N/A (arenas aren't arrays).

### Task 2 — recursive JIT survives Cheney GC (CORE LANDED, same arc)
The trampoline is no longer required for COMPILED execution: recursive (`RAVA_STACKFUL` unset) JIT
runs on the live arena and survives moving collection mid-frame.
- **`reload_after_gc`** (jit.rs): symmetric partner of `spill_for_gc` — re-reads forwarded arena
  pointers from the rooted buffer back into JIT locals (`def_var`, SSA-safe across diamonds) and ref
  operand-stack values, immediately after every GC-capable callback. Wired at all 7 reentry points.
- **`Vm::arena_collect_jit`** (lib.rs): recursive-path Cheney safepoint that forwards interp frames
  + statics AND the JIT cell buffers (`cells_all`) IN PLACE → a compiled frame's spilled arena
  pointers survive evacuation.
- **compiled `new` → arena**: `jit_new` routes through `new_object_value` (was the legacy handle
  `new_object` — that's why compiled code never touched the arena before); nursery-full self-collects
  + retries (no `RunExit::NurseryFull`, no `exit`). `callback_heavy` no longer treats arena `new` as
  heavy. All gated; **default byte-identical**.
- **PROVEN:** `Allocs` runs recursively through **206 live Cheney collects**, compiled-frame cell
  roots forwarded, **SUM byte-exact vs JVM, no segfault**. Non-allocating compiled hot loops now run
  JIT-engaged recursively: **Fields 994x→~4x, DispatchMono 3341x→~9x, ArithLoop ~3x**; Allocs/
  Collections correct but still interpreter-speed (their hot loop won't compile — `<init>`
  invokespecial is callback-heavy). Tests default **71** / flag **88**.
- **MC boot NOT reached** — orthogonal pre-existing blocker, NOT Task 2: the current `server.jar`
  (MC 1.21.x bundler, log4j 2.25.2) hits `unsupported native invokestatic org/apache/log4j/Logger.
  getLogger` (via netty's Log4JLoggerFactory) at `Starting net.minecraft.server.Main`, BEFORE
  block-state registration. Reproduced identically on the PRE-Task-2 flag binary AND under the
  trampoline → environmental/native-coverage, not a regression. Fix = support that native (and
  whatever follows) — separate from GC.
- **Residual gap:** the JIT-buffer root scan is conservative for type-erased local slots (a slot is
  an arena root iff 8-aligned + non-null + in-nursery) — tighten with precise stack maps. Also the
  recursive path forwards live INTERP frames via `*mut` while `&mut`-borrowed (GIL-synchronous, holds
  in practice; the deep fix is the stackful driver for pure-interp arena).
#### Inline TLAB bump — READY DESIGN (first target next session)
The headline speed item, deferred this session only because it needs compile-time class metadata
threaded through the `compile` signature (real plumbing, end-to-end untestable while MC is log4j-
blocked + `Allocs.<init>` won't inline). The mechanism is fully worked out:

1. **Cursor (nextgen.rs):** a thread-local `JIT_CURSOR: UnsafeCell<[u64;2]>` = `[next, end]` raw
   addresses, kept in lock-step with `GenHeap.young` (`next = base + young.top`, `end = base + cap`).
   - `jit_cursor_addr() -> i64`: ensure-init, sync cursor from `young`, return its stable address
     (called ONCE at the entry of any method containing an arena `new`; cache in an SSA value).
   - `sync_young_from_cursor(&mut GenHeap)`: `young.top = next - base` — call at the top of
     `tlab_try_alloc` + `tlab_collect` so interp/Rust allocs never overlap inline-bumped objects.
   - `refresh after alloc/collect`: republish `[base+top, base+cap]` into the cursor.
   (All four helpers were prototyped+reverted this session — re-add from git history of the reverted
   nextgen cursor block; they are correct.)
2. **Compile-time resolution:** build `new_arena: HashMap<u16,(u32 class_id,u32 nfields)>` in `jit_run`
   (like `fields`/`statics`) — walk bc for `0xbb`, resolve class name → arena-eligible? (the
   `new_object_value` predicate: pure-bytecode, non-enum, non-native-backed) → `register_class` +
   `class_field_layout().len()`. Thread into `Env` (new field) + `compile_with_static_intrinsics`
   signature (also update the test-only `compile()` shim).
3. **Inline bump IR (emit_op 0xbb), arena-eligible cp only:**
   ```
   retry:  next = load[cur]; end = load[cur+8]; nn = next + size(=HDR+nf*8)
           brif nn u<= end -> ok, overflow
   ok:     store[cur] = nn
           store[next+0] = (nf<<32)|class_id ; store[next+8]=0 ; store[next+16]=0   // header
           for i in 0..nf: store[next+24+i*8]=0                                      // zero cells
           result = next ; jump cont(result)
   overflow: spill_for_gc -> call rava_arena_safepoint() (sync→arena_collect_jit→refresh)
             -> reload_after_gc -> jump retry            // machine back-edge, no trampoline
   cont(phi): push result as Ref
   ```
   Non-arena cp stays on the `env.new_obj` callback (which already self-collects).
   This removes the per-alloc callback → allocating hot loops (`Allocs`, `Collections`) compile and
   should fall toward the sandbox ceiling.

- **Other next steps:** (2) back-edge STW safepoint poll — DEFERRED (a `load.i8` of a thread-local
  STW flag per loop back-edge is pure overhead until multi-thread STW exists; would regress the bench
  wins). (3) Precise GC stack maps to replace the conservative JIT-buffer scan (8-aligned + non-null +
  in-nursery) — needed for full soundness with type-erased local slots. (4) **Netty/log4j native
  blocker:** current `server.jar` (MC 1.21.x bundler) hits `unsupported native invokestatic
  org/apache/log4j/Logger.getLogger` via `io/netty/util/internal/logging/Log4JLoggerFactory` at
  `Starting net.minecraft.server.Main`, BEFORE block-state — pre-existing, reproduced on the pre-
  Task-2 binary AND under the trampoline (NOT a regression, NOT GC). Make unsupported-native dispatch
  THROW a catchable `LinkageError` (netty's logger probe try/catches it and falls back) instead of a
  fatal abort, to advance the recursive boot.

---

Memory-safety mandate stands: `RAVA_HARD_MB` backstop + `RAVA_PROC_CAP_MB` kernel cap. Recursive+
arena now survives GC for COMPILED frames (Task 2); pure-interpreter-recursive arena still prefers
the trampoline for snapshot/green-threads. Never break the default build.
