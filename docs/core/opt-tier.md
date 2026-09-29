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

## Tracer bullets

1. Leaf and loop methods with int/long/double arithmetic, locals, array and field access, and
   static calls without inlining. Guards deoptimize. Target: `BigInteger` loops and noise code.
2. Virtual and interface calls through the baseline's inline caches and itables. Counter-based
   trigger from baseline code.
3. Inlining with scope chains in frame states: stack traces expand inlined frames, and deopt
   resumes nested scopes.
4. Method-handle chains: `invokeBasic` and `linkTo*` on constant receivers are inlined.
