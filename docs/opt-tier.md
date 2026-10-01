# Tier 2: the optimizing compiler

## Why

The baseline JIT (`jit/`) keeps the operand stack in memory and every call pays a full frame.
Measured against HotSpot restricted to C1 (`-XX:TieredStopAtLevel=1`):

| | HotSpot (C2) | HotSpot C1 only | rava-core baseline |
|---|---:|---:|---:|
| `DfTree` (megamorphic function trees over doubles) | 112 ms | 138 ms | 283 ms |
| `BootLike` (records as map keys, method-handle chains) | 428 ms | 599 ms | ~3200 ms |
| `McLike` inside `main` | ~470 ms | 418 ms | ~700 ms |

A C1-class compiler is enough for parity. Every mechanism below is a generic JVM technique
(HotSpot has each of them); nothing is keyed to a workload.

## Shape

- **Trigger.** Baseline code counts invocations and taken back-edges. When a method gets hot in
  baseline code, it is recompiled by tier 2.
- **Frame.** Tier-2 frames keep the unified layout (`locals | header | operand stack` on the
  value stack, `r13` as the base, `r12` as the thread). Values live in registers. The memory
  slots are written only when the interpreter must take over. Stack walking, GC and exceptions
  therefore see the same frames as today.
- **IR** (`opt/ir.rs`): basic blocks of SSA values, with block parameters for phis. Values are
  typed (int, long, float, double, reference). The IR is built by abstract interpretation of
  the bytecode (`opt/build.rs`). Every instruction that can leave compiled code carries a
  *frame state*: bci, locals and stack as values, and the scope chain for inlined code.
- **Deoptimization** (`opt/deopt.rs`). A failed guard (null, bounds, class not initialized,
  unresolved constant, type-profile miss) calls the deopt stub with that frame state. The stub
  stores the values into the frame's slots, then the interpreter resumes the method at that bci
  and runs it to completion. Its back-edges can OSR into baseline code again. Inlined scopes are
  resumed innermost first: each returns into its caller's resumed state. A trap that keeps
  firing invalidates the tier-2 code.
- **Optimizations** (C1-like): constant folding, local value numbering, null-check elimination,
  inlining of statically bound and class-hierarchy-bound targets, and method-handle constants
  followed into their `LambdaForm`s. Intrinsics come from `jit/intrinsics.rs`.
- **Register allocation**: linear scan over SSA live ranges. Values live across calls use
  callee-saved registers or spill slots on the native stack. Oops in native frames are found by
  the collector's conservative scan.

## Implementation (`crates/rava-core/src/jit/opt/`)

| File | Role |
|---|---|
| `ir.rs` | Typed SSA values, blocks, terminators, frame states (`State`, chained for inlined scopes) |
| `build.rs`, `build_ops.rs` | Abstract interpretation of bytecode per `Scope`; phis at merges; deopt at unsupported or unresolved instructions |
| `build_arr.rs` | Array accesses; bounds checks dropped for known lengths and index ranges, merged per block for constant indices (range-check smearing) |
| `build_mh.rs` | `invokedynamic`, `invokeBasic`, `linkTo*`, `MethodHandle.invoke*` (constant handles fold to direct calls) |
| `freq.rs` | Estimated executions per entry of each instruction from branch profiles (Wu-Larus propagation, loops capped at ~20 iterations), for C2-style frequency-based inlining |
| `inline.rs` | Inlining (C2's size limits by call-site frequency: hot sites up to 325 bytes, others 35, cold ones only trivial methods): direct targets, methods no loaded class overrides (class hierarchy analysis: a flag test that deoptimizes), vtable-guarded and profile-guarded ones with a call on the guard's failure path |
| `canon.rs`, `phis.rs` | Constant folding and known-branch removal; trivial phi aliases |
| `split.rs` | Live-range splitting at innermost loops by SSA copies (entry copies of values used in the loop and live after it, exit copies of values defined in it) |
| `regalloc.rs`, `moves.rs` | Liveness, linear scan over one interval per value (spill order: idle in the loop at hand, then loop-weighted use density), parallel moves on edges |
| `barrier.rs` | Card marks elided for stores initializing an object allocated earlier in the block (ReduceInitialCardMarks) |
| `dom.rs`, `nullcheck.rs` | Dominators and natural loops; dominated null checks dropped |
| `codegen*.rs` | x86-64 emission in the baseline frame protocol; intrinsics; calls with inline caches |
| `deopt.rs` | Deopt descriptors, the per-method common stub, resuming scopes innermost first |

Rules that keep it correct:

- **Frames**: compiled code keeps the baseline layout; a call stores its arguments where the
  interpreter would. Inlined bodies write no frame headers: before a call from one, the physical
  header's bytecode-index slot gets a pointer to a static `InlinedSite` (the inlined frames and
  their bytecode indexes, HotSpot's debug info at a return address), which stack walks expand.
- **Lazy anchor** (HotSpot's last Java frame): optimized code publishes `Thread::top` (its
  innermost header) only before calls out, and `Thread::sp` only before runtime calls, polls and
  deoptimization; it restores `top` on return. `sp` only ever rises while it runs (baseline code
  and the interpreter need it at or above their frame's end when a call returns), the adapters
  into the interpreter and natives raise it past the arguments, and Rust callers restore it.
  The prologue writes METHOD, BCI and CALLER only (no SAVED_SP; deoptimization writes one for
  code that takes the frame over).
- **Deoptimization** finishes the method in the interpreter (`interp::resume`); an exception in a
  callee deoptimizes with it pending, so no exception handler is ever compiled. Frequent deopts
  recompile the method (constants resolved since), then fall back to baseline tier-2 code.
  At the threshold the code is retired to baseline at once (HotSpot's not-entrant), so calls stop
  deoptimizing while the recompilation waits in the queue.
- **Speculation and trap history** (`jit/profile.rs`): every deopt records its (method, bci);
  recompilations do not speculate there again. Speculations: a complete receiver profile
  deoptimizes on an unseen class; a branch direction first-tier code never took (64+ runs)
  deoptimizes (first-tier `branch()` counts both directions); a block from which every path
  throws deoptimizes on entry (`analysis::throw_paths`), so exception construction is not
  compiled or inlined.
- **Early resolution**: constant-pool entries of classes the loader already has (and their
  fields and methods) are resolved at compile time (`Klass::peek_resolved`, as HotSpot's ciEnv).
- **Baseline code stays** (`Method::baseline`): exception dispatch and loop entry (OSR) of frames
  still running baseline code, and interpreted loops after a deoptimization, use it.
- **Class hierarchy analysis**: `Method::mark_overridden` runs while an overriding class is
  linked, before any instance of it exists; code that inlined the method without a receiver
  check tests that flag and deoptimizes once it is set, then recompiles with a vtable guard.
- **Frameless leaves**: code without loops, calls, allocation, throwing checks, monitors or
  deopts writes no frame header and skips the stack check and entry poll (nothing can walk or
  stop it while it runs).
- **GC**: values live in registers and native spill slots, which the conservative stack scan
  covers; the collector never moves objects.

Debugging: `RAVA_CORE_OPT=0` (baseline tier 2 only), `RAVA_CORE_OPT=all` (compile everything
with this tier), `RAVA_CORE_OPT_LOG`, `RAVA_CORE_OPT_IR=<method>` (IR and allocation), `RAVA_CORE_OPT_SPLIT=0`, `RAVA_CORE_INLINE_LOG`,
`RAVA_CORE_DEOPT_LOG`, `RAVA_CORE_JIT_DUMP=<method>`, `RAVA_CORE_JIT_STATS`.

## Next

1. SSE registers for float and double values (noise code moves them through GPRs today).
2. Value numbering of pure instructions and field loads along dominating paths.
3. Background compilation (compiles run on the thread that got hot today).
4. On-stack replacement entries into optimized code.
