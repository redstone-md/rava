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
| `build_mh.rs` | `invokedynamic`, `invokeBasic`, `linkTo*`, `MethodHandle.invoke*` (constant handles fold to direct calls) |
| `inline.rs` | Inlining (baseline policy at tier 2): direct targets, vtable-guarded and profile-guarded ones with a call on the guard's failure path |
| `canon.rs`, `phis.rs` | Constant folding and known-branch removal; trivial phi aliases |
| `regalloc.rs`, `moves.rs` | Liveness, linear scan over one interval per value, parallel moves on edges |
| `codegen*.rs` | x86-64 emission in the baseline frame protocol; intrinsics; calls with inline caches |
| `deopt.rs` | Deopt descriptors, the per-method common stub, resuming scopes innermost first |

Rules that keep it correct:

- **Frames**: compiled code keeps the baseline layout; a call stores its arguments where the
  interpreter would, and visible inlined frames are linked into `Thread::top` around calls, so
  stack walks and exceptions see every Java frame.
- **Deoptimization** finishes the method in the interpreter (`interp::resume`); an exception in a
  callee deoptimizes with it pending, so no exception handler is ever compiled. Frequent deopts
  recompile the method (constants resolved since), then fall back to baseline tier-2 code.
- **Baseline code stays** (`Method::baseline`): exception dispatch and loop entry (OSR) of frames
  still running baseline code, and interpreted loops after a deoptimization, use it.
- **GC**: values live in registers and native spill slots, which the conservative stack scan
  covers; the collector never moves objects.

Debugging: `RAVA_CORE_OPT=0` (baseline tier 2 only), `RAVA_CORE_OPT=all` (compile everything
with this tier), `RAVA_CORE_OPT_LOG`, `RAVA_CORE_OPT_IR=<method>`, `RAVA_CORE_INLINE_LOG`,
`RAVA_CORE_DEOPT_LOG`, `RAVA_CORE_JIT_DUMP=<method>`, `RAVA_CORE_JIT_STATS`.

## Next

1. SSE registers for float and double values (noise code moves them through GPRs today).
2. Value numbering of pure instructions and field loads along dominating paths.
3. Background compilation (compiles run on the thread that got hot today).
4. On-stack replacement entries into optimized code.
