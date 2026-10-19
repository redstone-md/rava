# Tier 2: the optimizing compiler

## Why

The baseline JIT (`jit/`) keeps the operand stack in memory, and every call pays for a full
frame. Against HotSpot restricted to C1 (`-XX:TieredStopAtLevel=1`):

| | HotSpot (C2) | HotSpot C1 only | rava baseline |
|---|---:|---:|---:|
| `DfTree` (megamorphic function trees over doubles) | 112 ms | 138 ms | 283 ms |
| `BootLike` (records as map keys, method-handle chains) | 428 ms | 599 ms | ~3200 ms |
| `McLike` inside `main` | ~470 ms | 418 ms | ~700 ms |

A C1-class compiler is enough for parity. Every mechanism below is a generic JVM technique
that HotSpot also has. None of it is keyed to a workload.

## Shape

- Baseline code counts invocations and taken back-edges. A method that gets hot in baseline
  code is recompiled by tier 2.
- Tier-2 frames keep the unified layout (`locals | header | operand stack` on the value stack,
  `r13` as the base, `r12` as the thread). Values live in registers, and the memory slots are
  written only when the interpreter has to take over. Stack walking, GC and exceptions therefore
  see the same frames they see in baseline code.
- The IR (`opt/ir.rs`) is basic blocks of SSA values, with block parameters for phis. Values
  are typed (int, long, float, double, reference). The IR is built by abstract interpretation
  of the bytecode (`opt/build.rs`). Each instruction that can leave compiled code carries a
  *frame state*: the bci, the locals and stack as values, and the scope chain for inlined code.
- Deoptimization (`opt/deopt.rs`). A failed guard (null, bounds, class not initialized,
  unresolved constant, type-profile miss) calls the deopt stub with that frame state. The stub
  stores the values into the frame's slots. The interpreter then resumes the method at that bci
  and runs it to completion; its back-edges can OSR into baseline code again. Inlined scopes are
  resumed innermost first, each returning into its caller's resumed state. A trap that keeps
  firing invalidates the tier-2 code.
- Optimizations are C1-like: constant folding, local and global value numbering, null-check
  elimination, inlining of statically bound and class-hierarchy-bound targets, and method-handle
  constants followed into their `LambdaForm`s. Intrinsics come from `jit/intrinsics.rs`.
- Register allocation is a linear scan over SSA live ranges. Values live across calls go to
  callee-saved registers or to spill slots on the native stack. The collector's conservative
  scan finds oops in native frames.

## Implementation (`crates/rava/src/jit/opt/`)

| File | Role |
|---|---|
| `ir.rs` | Typed SSA values, blocks, terminators, frame states (`State`, chained for inlined scopes) |
| `build.rs`, `build_ops.rs` | Abstract interpretation of bytecode per `Scope`; phis at merges; deopt at unsupported or unresolved instructions |
| `build_arr.rs` | Array accesses; bounds checks dropped for known lengths and index ranges, merged per block for constant indices (range-check smearing) |
| `build_mh.rs` | `invokedynamic`, `invokeBasic`, `linkTo*`, `MethodHandle.invoke*` (constant handles fold to direct calls) |
| `freq.rs` | Estimated executions per entry of each instruction from branch profiles (Wu-Larus propagation, loops capped at ~20 iterations), for C2-style frequency-based inlining |
| `inline.rs` | Inlining (C2's size limits by call-site frequency: hot sites up to 325 bytes, others 35, cold ones only trivial methods): direct targets, methods no loaded class overrides (class hierarchy analysis: a flag test that deoptimizes), vtable-guarded and profile-guarded ones with a call on the guard's failure path |
| `canon.rs`, `phis.rs` | Constant folding and known-branch removal; trivial phi aliases |
| `gvn.rs` | Global value numbering of pure arithmetic, conversions and compares after unrolling: a repeat of a dominating instruction becomes its alias |
| `split.rs` | Live-range splitting at innermost loops by SSA copies (entry copies of values used in the loop and live after it, exit copies of values defined in it) |
| `regalloc.rs`, `moves.rs` | Liveness, linear scan over one interval per value (spill order: idle in the loop at hand, then loop-weighted use density), parallel moves on edges |
| `barrier.rs` | Card marks elided for stores initializing an object allocated earlier in the block (ReduceInitialCardMarks) |
| `dom.rs`, `nullcheck.rs` | Dominators and natural loops; dominated null checks dropped |
| `codegen*.rs` | x86-64 emission in the baseline frame protocol; intrinsics; calls with inline caches |
| `deopt.rs` | Deopt descriptors, the per-method common stub, resuming scopes innermost first |
| `osr.rs` | On-stack replacement: versions compiled from a loop header (live locals read from the frame as parameters, kinds by verifier-style inference), entered from first-tier back edges through a bridge that calls the body on the running frame |

Rules that keep it correct:

- Frames. Compiled code keeps the baseline layout. A call stores its arguments where the
  interpreter would, except the ones passed in registers (next item). Inlined bodies write no
  frame headers. Before a call from an inlined body, the bytecode-index slot of the physical
  header gets a pointer to a static `InlinedSite`, which lists the inlined frames and their
  bytecode indexes (HotSpot keeps the same debug info at a return address). Stack walks expand it.
- Register arguments (`jit/abi.rs`, a small version of HotSpot's compiled calling convention).
  A direct call from optimized code passes the first four integer and reference arguments in
  rdi, r8, r9, r10 and the first four float and double ones in xmm2-xmm5. The rest go to the
  callee's frame slots as before. The call goes to the callee's register trampoline. That
  reaches the register entry of the callee's current optimized code, or an adapter that stores
  the registers into the frame and enters whatever code the method has (interpreter, baseline,
  code without a register entry). The ordinary entry of optimized code loads the register
  arguments from the frame and falls into its register entry, which is aligned to 16 bytes. The
  prologue stores register arguments into their frame slots wherever deoptimization or a spill
  reads them, so frames stay what stack walks, the collector and deoptimization expect.
  Vtable and interface calls pass registers too. Once they find the target, they call its
  `Method::reg_call` (where its register trampoline points); a shared stub creates it on first
  use and keeps the argument registers intact. The register allocator prefers the argument
  register for a parameter and for an argument that dies at the call (`opt/hints.rs`). Calls
  through method handles, profiling sites, OSR versions and synchronized methods use the frame
  protocol.
- Fast exits (`opt/codegen/fastexit.rs`, a small form of shrink-wrapping). Suppose the entry
  block ends in a test, and one side of it only computes simple integer values (or reads fields
  of the receiver) and returns. Then that test and that side run at the register entry, before
  the prologue, using only the argument registers and scratch registers. A recursion's base
  case or the return of a cached value pays no pushes, stack check, frame header or entry poll.
  Otherwise the code falls into the prologue with the entry registers untouched.
- Lazy anchor (HotSpot's last Java frame). Optimized code publishes `Thread::top` (its
  innermost header) only before calls out, and `Thread::sp` only before runtime calls, polls and
  deoptimization. It restores `top` on return. While it runs, `sp` only rises: baseline code and
  the interpreter need it at or above their frame's end when a call returns. The adapters into
  the interpreter and natives raise it past the arguments, and Rust callers restore it. The
  prologue writes METHOD, BCI and CALLER only. It writes no SAVED_SP; deoptimization writes one
  for code that takes the frame over.
- Deoptimization finishes the method in the interpreter (`interp::resume`). An exception in a
  callee deoptimizes with the exception pending, so no exception handler is ever compiled.
  Frequent deopts recompile the method (with constants resolved since then), and after that the
  method falls back to baseline tier-2 code. At the threshold the code is retired to baseline at
  once (HotSpot's not-entrant), so calls stop deoptimizing while the recompilation waits in the
  queue.
- Speculation and trap history (`jit/profile.rs`). Every deopt records its (method, bci), and
  recompilations do not speculate there again. The speculations are: a complete receiver
  profile deoptimizes on an unseen class; a branch direction first-tier code never took (64+
  runs) deoptimizes (first-tier `branch()` counts both directions); a block from which every
  path throws deoptimizes on entry (`analysis::throw_paths`), so exception construction is
  neither compiled nor inlined.
- Early resolution. Constant-pool entries for classes the loader already has, and for their
  fields and methods, are resolved at compile time (`Klass::peek_resolved`, like HotSpot's
  ciEnv).
- Baseline code stays (`Method::baseline`). Exception dispatch and loop entry (OSR) of frames
  still running baseline code use it, and so do interpreted loops after a deoptimization.
- OSR into optimized code (`osr.rs`, HotSpot's OSR compilations). Optimized code has no block
  entries, so a hot first-tier loop asks for a version of its method built from the loop's
  header. Only what the header reaches is built; loops around it become ordinary cycles. Its
  deoptimizations count against that version, which is retired after 8 and rebuilt up to 4
  times. Synchronized methods are not entered this way.
- Class hierarchy analysis. `Method::mark_overridden` runs while an overriding class is being
  linked, before any instance of it exists. Code that inlined the method without a receiver
  check tests that flag, deoptimizes once it is set, and then recompiles with a vtable guard.
- Frameless leaves. Code without loops, calls, allocation, throwing checks, monitors or deopts
  writes no frame header and skips the stack check and the entry poll. Nothing can walk or stop
  it while it runs.
- GC. Values live in registers and native spill slots, and the conservative stack scan covers
  both. The collector never moves objects.
- Saves around calls follow block liveness (`liveness::live_after_calls`). A linear-scan
  interval also covers blocks laid out between a definition and a use on another path. A
  value dead after a call is therefore not saved; one read only by the call's own state is
  restored only on the way to its deoptimization.
- Interface type checks (`instanceof`, `checkcast`) compare the class's last hit, then probe
  the class's interface set (`Klass::iface_set`): an open-addressed table at most half full,
  probed from a hash of the interface that is a constant in the code. A miss ends at an empty
  slot after one or two probes. HotSpot 21 scans its secondary supers linearly, and so did
  this tier, so every "no" answer read all of a class's interfaces (dozens for modded entity
  classes).
- Exact receiver types. Behind a class guard, the inlined body's receiver has exactly that
  class, so its own calls on `this` dispatch statically (C2's type-based devirtualization).
  This matters most where a profile is shared and polluted: the profile of an interface's
  default method holds every implementation that ran it, world generation included.
- `StringLatin1.equals` is compared inline: lengths, then eight bytes at a time, the last
  word again overlapping, and a short array's single word with the bytes past its end
  shifted out.

Debugging: `RAVA_OPT=0` (baseline tier 2 only), `RAVA_OPT=all` (compile everything
with this tier), `RAVA_OPT_LOG`, `RAVA_OPT_IR=<method>` (IR and allocation), `RAVA_OPT_SPLIT=0`, `RAVA_INLINE_LOG`,
`RAVA_DEOPT_LOG`, `RAVA_JIT_DUMP=<method>`, `RAVA_JIT_STATS`.

Profiling (Windows, `sampler.rs`): `RAVA_SAMPLE=<file>` samples every Java thread's pc each
millisecond. `RAVA_SAMPLE_WINDOW=1` also writes each ten seconds on its own (`<file>.w<secs>`).
`RAVA_SAMPLE_DETAIL=<name part>` counts compiled methods per instruction offset and records
the instruction's bytes; `/` matches every method. Grouping those rows by instruction shape
across methods finds a pattern repeated everywhere (the interface scan above was one).
`RAVA_SAMPLE_STACK=1` writes folded stacks from the frame headers, inlined frames included
(`<file>.stacks.w<secs>`), for inclusive comparison with a JFR recording.

## Next

1. More allocatable registers. The allocator has nine general registers, plus rcx and rdx for
   values that avoid the instructions writing them; rax and r11 are code generation's scratch.
   C2 allocates about thirteen, and loops with many live values (formatting, regex matching)
   spill where C2 does not. Freeing r11 means code generation stops taking it for granted.
2. Live-range splitting around calls, so a value used in a loop with a rare call keeps a
   caller-saved register on the hot path.
