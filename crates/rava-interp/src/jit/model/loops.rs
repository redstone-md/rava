//! Loop plans: what an analysis hands the emitter once it has admitted a shape.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// LOOP-FOLDING plan (next-gen-gc): a whole method recognised as
/// `acc = C; for (i = C0; i < LIM; i++) acc += INV; return acc` where `INV` is a
/// loop-INVARIANT `long` value (computed from getstatic + int constants + int
/// arithmetic only — never `i`, never `acc`, no side effects). Such a loop folds to
/// the closed form `acc = C + INV * max(0, LIM - C0)` (wrapping i64 — associativity
/// of wrapping addition makes the closed form bit-exact with the iterated sum). The
/// trip count is computed at runtime (LIM is usually a parameter); `inv_range` is
/// the body's invariant-expression bytecode, emitted ONCE.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct LoopFold {
    pub acc_init: i64,
    pub c0: i32,
    /// Loop limit source: `Some(local)` = `iload local`; `None` = the `lim_const`.
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    /// Bytecode range [start, end) of the invariant int expression (after the body's
    /// `lload acc`, before `i2l`). Emitted once to compute `S`.
    pub inv_range: (usize, usize),
}

/// Minimum trip count for auto-parallelism to pay (fork + reduce overhead). A
/// compile-time-constant limit below this is rejected; a runtime limit becomes a
/// guard the forker emits (`if (lim - c0)/step >= MIN_PARALLEL_TRIP`).
#[cfg(feature = "next-gen-gc")]
pub(crate) const MIN_PARALLEL_TRIP: i64 = 10_000;

/// A counted associative-accumulator loop whose iterations are INDEPENDENT — the
/// shape the JIT forks across `RavaPool` (Kit 3). Unlike `LoopFold` (where the body
/// is loop-INVARIANT → closed form), here the per-iteration expression may depend on
/// the counter `i`; what makes the fork sound is (a) the only loop-carried state is
/// the accumulator, folded by an ASSOCIATIVE op, and (b) the body performs NO shared
/// mutation (no putfield/putstatic/array-store/call) — only counter-dependent integer
/// arithmetic. Each worker folds a sub-range into a partial; partials reduce via `op`.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct DataParallelLoop {
    pub iv_local: usize,
    pub acc_local: usize,
    pub acc_is_long: bool,
    pub acc_init: i64,
    pub c0: i32,
    pub step: i32,
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    /// When the loop limit is a `int n = arr.length;` hoist, the array local whose
    /// length IS the limit. The parallel codegen replaces the method (so it never runs
    /// that prologue) and must resolve the trip from this array's runtime length rather
    /// than the never-initialised limit local. `None` for a param/constant limit.
    pub limit_from_array: Option<usize>,
    pub op: crate::parallel_pool::ReduceOp,
    /// Bytecode range [start, end) of the per-iteration expression (after `load acc`,
    /// before the optional `i2l` / the fold op). Compiled into the worker body.
    pub body_range: (usize, usize),
    /// Invariant external locals (params) read inside the body — `(local_index,
    /// is_long)`, in first-seen order. The fork marshals their values into a
    /// `WorkerEnv` array; the body reads them at `env[slot*8]` (slot = position here).
    pub captures: Vec<(usize, bool)>,
    /// Body-local SCRATCH slots (a javac temp written + read inside the body) —
    /// `(local_index, is_long)`. Each maps to an isolated Cranelift `Variable` in the
    /// body fn (and the serial fallback), re-initialised each iteration.
    pub scratch: Vec<(usize, bool)>,
    /// Captured primitive-array locals read via `aload L; iload iv; (i/l)aload` inside
    /// the body — `(local_index, elem_is_long)`. The fork resolves each array handle to
    /// a raw element base pointer (under the GIL, before forking) and marshals it into
    /// the worker env after the scalar captures; the worker loads `base[i]` directly
    /// (no heap access, no bounds check — the fork gate proved `trip <= length`).
    pub array_captures: Vec<(usize, bool)>,
    /// Bytecode pc of the fold's `load acc` (inside `body_range`) — the body emitter
    /// skips it (the accumulator is supplied by the fold, not the body value).
    pub acc_load_pc: usize,
    /// When the fold is `invokestatic Math.max/min` (not a bytecode fold op), the CP index
    /// of that methodref — the dispatch resolves it (it has the pool) to `ReduceOp::Max/Min`
    /// or rejects. `None` for an opcode fold (op is final).
    pub fold_static_cp: Option<u16>,
    /// When the accumulator seed is a `ldc2_w` long constant, its CP index — the dispatch
    /// resolves the value into `acc_init` (the analyzer has no pool). `None` otherwise.
    pub acc_init_cp: Option<u16>,
}

/// A counted ELEMENT-INDEPENDENT MAP loop `for (i=c0; i<LIM; i++) out[i] = f(in[i], …)`
/// — no accumulator, no cross-iteration state. Each iteration writes ONLY its own `out[i]`
/// (index == the bare counter → provably disjoint), reading `in[j][i]` captured arrays +
/// invariant params. The JIT forks it across `RavaPool` via `parallel_map`: workers write
/// disjoint index ranges directly (no combine, order-free). The method returns void.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct MapLoop {
    pub iv_local: usize,
    pub c0: i32,
    pub step: i32,
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    pub limit_from_array: Option<usize>,
    /// The OUTPUT array local (`aload out; iload iv; <value>; (i/l)astore`). Resolved to a
    /// writable element base before the fork; the worker stores `out[i]` directly.
    pub out_local: usize,
    /// `true` = `lastore` (long elements), `false` = `iastore` (int elements).
    pub out_is_long: bool,
    /// Bytecode range [start, end) of the loop body (body start → the terminal astore).
    /// The `store_skip` pcs (the store target) are excluded by the emitter so the body
    /// leaves just the stored value on the stack.
    pub body_range: (usize, usize),
    /// The store target `aload out; iload iv` pcs (skipped by the body emitter — the fork
    /// resolves the output base + writes out[i] itself).
    pub store_skip: Vec<usize>,
    pub captures: Vec<(usize, bool)>,
    pub scratch: Vec<(usize, bool)>,
    pub array_captures: Vec<(usize, bool)>,
    /// `invokestatic` sites in the value (pc → CP) — the dispatch resolves each to
    /// Math.max/min (→ `math_ops`) or rejects the whole map.
    pub static_calls: Vec<(usize, u16)>,
    /// Resolved Math.max/min sites (pc → is_max), filled by the dispatch; smax/smin.
    pub math_ops: Vec<(usize, bool)>,
    /// STENCIL offset range: min/max constant offset an input array is read at relative to
    /// the counter (`in[i+off]`). `(0,0)` for a plain `in[i]` map. The fork gate tightens the
    /// input bounds by these (`c0+lo>=0`, `limit+hi<=length`) so every neighbour access is
    /// in-bounds with no per-element check.
    pub read_off_lo: i32,
    pub read_off_hi: i32,
    /// `ldc2_w` sites in the value (pc → CP) — the dispatch resolves each to a long/double
    /// constant (→ `const_ops`) or rejects.
    pub const_calls: Vec<(usize, u16)>,
    /// Resolved `ldc2_w` values (pc → long value / double bit-pattern), filled by dispatch.
    pub const_ops: Vec<(usize, i64)>,
}

/// M3 (F1) — an OBJECT-BUILDING for-each MAP: `for (i=c0; i<LIM; i++) out[i] = f(src[i])`,
/// where `f` is a per-element method (the element as receiver `src[i].f()`, or as the sole
/// argument `f(src[i])`) producing a fresh REFERENCE result stored into a disjoint slot of a
/// distinct output array. This is the reference-typed sibling of `analyze_data_parallel_map`:
/// the numeric map only admits primitive arithmetic bodies (`map_stack_delta` rejects calls),
/// but the shape-cache build applies an arbitrary per-element method that ALLOCATES + calls.
/// Its soundness rests NOT on a numeric-only body but on the escape analysis in `offload.rs`
/// proving `f`'s whole subtree side-effect-free (reads immutable state, allocates fresh) — so
/// this analyzer only recognises the loop SKELETON + the call site; the dispatch gates the
/// fork on `is_offloadable(f)` (and F2 devirtualisation of `f`'s internal abstract calls).
///
/// Rigid by design: the body is EXACTLY `aload out; iload i; aload src; iload i; aaload;
/// [checkcast]?; invoke f; [checkcast]?; aastore`. Because valid bytecode balances the stack,
/// this instruction sequence PROVES `f` consumes precisely the one element (a stray extra arg
/// would need another load between `aaload` and `invoke`, which the sequence forbids) and
/// leaves exactly one reference for the store — so no descriptor is needed at analysis time.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) struct ForkEachLoop {
    pub iv_local: usize,
    pub c0: i32,
    pub step: i32,
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    /// When the limit is a hoisted `int n = arr.length`, the array local (the fork resolves the
    /// trip from its runtime length — the replaced method never runs the hoist prologue).
    pub limit_from_array: Option<usize>,
    /// Source array local: the element is `src[i]` (an `aaload`).
    pub src_local: usize,
    /// Output array local: the disjoint store `out[i] = f(src[i])` (index == counter → no merge).
    pub out_local: usize,
    /// The per-element method applied to `src[i]`. CP index + invoke opcode; the dispatch
    /// resolves it against the pool and gates the fork on `is_offloadable`.
    pub call_cp: u16,
    pub call_op: u8,
    pub call_pc: usize,
    /// `[start, end)` — body start (the store target `aload out`) up to the terminal `aastore`.
    pub body_range: (usize, usize),
}

/// M3 (F3.3) — the ITERATOR for-each loop `Iterator it = coll.iterator(); while (it.hasNext())
/// { E e = (E) it.next(); e.m(); }` — the shape the block-state `initCache` build actually uses
/// (a nested `for (BlockState s : states) s.initCache()`, compiled to iterator()/hasNext()/
/// next() rather than `forEach` or a counted loop). This is the RUNTIME fork target: at the
/// `iterator()` call the collection + the concrete element type are known, so the per-element
/// method `m` can be element-ownership-devirtualised and the whole loop forked (drain + apply,
/// then the interpreted loop sees an empty iterator and spins zero times — no control-flow
/// surgery). This matcher is PURE over the caller bytecode starting at the `iterator()` invoke;
/// it recognises the loop SKELETON only (the wiring resolves + verifies iterator/hasNext/next by
/// name against the pool). `None` on any deviation.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) struct IterForEach {
    /// Local the iterator is stored into (`astore it`).
    pub it_local: usize,
    /// Local each element is stored into (`astore e`).
    pub e_local: usize,
    /// The per-element method applied to `e` (`e.m()`): invoke opcode + CP index.
    pub elem_op: u8,
    pub elem_cp: u16,
    /// pc just past the loop (the `ifeq` exit target) — where interpretation resumes.
    pub loop_end_pc: usize,
    /// `true` if `e.m()` returns non-void and its result is popped (`pop`/`pop2`).
    pub elem_result_popped: bool,
}

/// M3 (F3.3c) — the SPLIT iterator for-each: `for (E e : coll) { <prefix(e)>; e.m(); }` where the
/// body is a per-element PREFIX of straight-line statements (e.g. the sequential, order-dependent
/// `Block.BLOCK_STATE_REGISTRY.add(s)` id-assignment) followed by the offloadable per-element call
/// `e.m()` (e.g. `s.initCache()`). This is T1's ACTUAL shape (the strict single-statement matcher
/// correctly rejects it). The fork PLAN: run the prefix serially per element IN ORDER (pass 1),
/// then run `e.m()` in parallel (pass 2) — sound iff `e.m()` is reorder-independent of the prefix,
/// which the HASH-bit-exact boot is the arbiter of. The prefix is required branch-free and must
/// not overwrite the it/e locals, so it can be replayed per element from a synthetic frame.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) struct IterForEachSplit {
    pub it_local: usize,
    pub e_local: usize,
    /// Bytecode range `[start, end)` of the per-element prefix statements (before `aload e; e.m()`).
    pub prefix_range: (usize, usize),
    pub elem_op: u8,
    pub elem_cp: u16,
    pub loop_end_pc: usize,
    pub elem_result_popped: bool,
}

/// M3 T2 (step 1) — a GENERAL counted loop `for (int i = lo; i < hi; i += step) { <body> }` in the
/// bottom-test shape javac emits. Unlike `ScanLoop` (a fixed `out[i]=op(acc,in[i])` array fold),
/// the body is arbitrary — this is the shape worldgen column/cell scans take (numeric range, not
/// `collection.forEach`), which the iterator matcher cannot see. DETECTION ONLY for now: it
/// captures the induction var, bound, step and body byte-range so a later step can check the body
/// offloadable and fork iterations `[lo,hi)` across workers. `bound` is a local or an int constant.
#[cfg(feature = "next-gen-gc")]
#[derive(Debug, PartialEq)]
pub(crate) enum RangeBound {
    Local(usize),
    Const(i32),
}

#[cfg(feature = "next-gen-gc")]
#[derive(Debug, PartialEq)]
pub(crate) struct NumericRangeLoop {
    /// Induction variable local.
    pub iv_local: usize,
    /// Upper bound (exclusive for `<`, inclusive for `<=`).
    pub bound: RangeBound,
    /// `true` if the compare is `<=` (if_icmpgt exits) rather than `<` (if_icmpge exits). Here we
    /// match the bottom-test back-edge form: if_icmplt (`<`) / if_icmple (`<=`) jumps to the body.
    pub inclusive: bool,
    /// `iinc` step amount (must be positive).
    pub step: i32,
    /// Body byte range `[start, end)` — `end` is the `iinc` pc (body is everything before the step).
    pub body_start_pc: usize,
    pub body_end_pc: usize,
    /// The condition block pc (target of the leading `goto`).
    pub cond_pc: usize,
    /// pc just past the loop (fallthrough of the back-edge compare) — where interpretation resumes.
    pub loop_exit_pc: usize,
}

/// M3 10.2a — a 3-nested COORDINATE MAP loop `for x { for z { for y { d = RECV.m(new CTX(x,y,z)); acc =
/// reduce(acc, d); }}}`. The embarrassingly-parallel chunk-noise-fill idiom: iterations independent (no
/// loop-carried state but the reduction accumulator + the counters), the per-point payload is a
/// value-returning call on a loop-INVARIANT receiver whose only arg is a context built from the 3 loop
/// counters. Structural (pattern-generic — no class-name hardcode here; the caller confirms the CTX/RECV
/// signatures via the pool). `ctx_cp`/`compute_cp` are the `new`/`invokeinterface` constant-pool indices.
#[cfg(feature = "next-gen-gc")]
#[derive(Debug)]
pub(crate) struct CoordMapLoop {
    pub x_iv: usize,
    pub z_iv: usize,
    pub y_iv: usize,
    pub x_bound: RangeBound,
    pub z_bound: RangeBound,
    pub y_bound: RangeBound,
    pub ctx_cp: u16,       // `new` CTX class (the SinglePointContext)
    pub compute_cp: u16,   // invokeinterface RECV.compute(ctx):D
    pub acc_local: usize,  // the reduction accumulator (long)
    pub body_start_pc: usize,
    pub body_end_pc: usize,
    pub loop_exit_pc: usize,
    // 10.2b serial-split extraction:
    pub recv_local: usize,     // the loop-INVARIANT receiver (aload before `new` — `density`)
    pub ymin_local: usize,     // Y-loop init source local (yMin)
    pub reduce_mul_cp: u16,    // ldc2_w constant C in `acc = acc*C + doubleToRawLongBits(d)`
    pub compute_end_pc: usize, // pc just after the invokeinterface compute (start of the reduction)
}

/// A counted PREFIX-SCAN loop `for (i=c0; i<LIM; i++) { acc = op(acc, in[i]); out[i] = acc; }`
/// over LONG arrays, with `acc` seeded to `op.identity()`. Cross-iteration (out[i] depends on
/// out[i-1]) so it forks via the 2-pass `parallel_scan` engine — the driver just resolves the
/// in/out element bases and hands them off; there's no per-element worker body (the value is
/// exactly `in[i]`). The method returns void.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct ScanLoop {
    pub iv_local: usize,
    pub c0: i32,
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    pub in_local: usize,
    pub out_local: usize,
    pub op: crate::parallel_pool::ReduceOp,
    /// 8 for a long[] scan, 4 for an int[] scan — the element stride the engine reads/writes.
    pub elem_bytes: i64,
}

/// A counted FILTER / stream-compaction loop `int k=0; for(i=c0;i<LIM;i++){ if(in[i] <cmp> C)
/// { out[k]=in[i]; k++; } } return k;` over LONG arrays. `pred_keep` is the KEEP comparison
/// (element `cmp` C), `pred_cp` the constant-pool index of C (ldc2_w). Forks via the 2-pass
/// `parallel_filter` engine with a JIT-emitted predicate fn; returns the surviving count.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct FilterLoop {
    pub iv_local: usize,
    pub k_local: usize,
    pub c0: i32,
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    pub in_local: usize,
    pub out_local: usize,
    pub pred_keep: IntCC,
    pub pred_cp: u16,
    pub pred_const: i64,
    pub elem_bytes: i64,
}

/// A counted COUNT-where-predicate loop `int c=0; for(i=c0;i<LIM;i++){ if(in[i] <cmp> C) c++; }
/// return c;`. Embarrassingly parallel — forks via `parallel_count` with a JIT-emitted predicate.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct CountLoop {
    pub iv_local: usize,
    pub c_local: usize,
    pub c0: i32,
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    pub in_local: usize,
    pub pred_keep: IntCC,
    pub pred_cp: u16,
    pub pred_const: i64,
    pub elem_bytes: i64,
}

/// A counted ARGMAX loop `long max=a[0]; int best=0; for(i=1;i<LIM;i++){ if(a[i]>max){max=a[i];
/// best=i;} } return best;` over a LONG array. The seed `a[0]` extends the considered range to
/// `[0, LIM)`, so the fork scans `trip = LIM` elements via `parallel_argmax` (ties → lowest
/// index, matching the guest's strict `>`).
#[cfg(feature = "next-gen-gc")]
pub(crate) struct ArgmaxLoop {
    pub arr_local: usize,
    pub iv_local: usize,
    pub max_local: usize,
    pub best_local: usize,
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    pub elem_bytes: i64,
    /// `true` = index of the max (ifle), `false` = index of the min (ifge).
    pub want_max: bool,
}

/// A counted DUAL-accumulator reduction `long acc1=0, acc2=0; for(i=c0;i<LIM;i++){ acc1 =
/// op1(acc1, <v1>); acc2 = op2(acc2, <v2>); } return <expr(acc1,acc2)>;` over LONG accumulators
/// — one fused pass computing two associative folds (e.g. sum + sum-of-squares → variance).
/// `val1_range`/`val2_range` are the per-fold value expressions (over the counter + arrays);
/// `ret_range` is the post-loop return expression (over acc1/acc2 + constants only).
#[cfg(feature = "next-gen-gc")]
pub(crate) struct Reduce2Loop {
    pub acc1_local: usize,
    pub acc2_local: usize,
    pub iv_local: usize,
    pub c0: i32,
    pub limit_local: Option<usize>,
    pub lim_const: i32,
    pub op1: crate::parallel_pool::ReduceOp,
    pub op2: crate::parallel_pool::ReduceOp,
    pub val1_range: (usize, usize),
    pub val2_range: (usize, usize),
    pub ret_range: (usize, usize),
    /// Input arrays read as `a[i]` across both values — `(local, elem_is_long)`.
    pub array_captures: Vec<(usize, bool)>,
    /// Captured scalar params read in the values — `(local, is_long)`.
    pub captures: Vec<(usize, bool)>,
    /// `true` = long accumulators (laload/ladd/lreturn), `false` = int (iaload/iadd/ireturn).
    pub acc_is_long: bool,
}
