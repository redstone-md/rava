# Copy-and-Patch tier — roadmap

Goal (user, 2026-07-01): build copy-and-patch → measure overhead → then AOT + C2 quality.

## Why (measured)
Boot is COLD-code-interpretation bound (many methods called < the Cranelift compile
threshold; interpreted at ~2-3M op/s). Cranelift is too expensive to compile the cold bulk
(proven: THRESHOLD=128 and opt=none both fail to reach Done in time). Copy-and-patch
(Xu & Kjolstad PLDI'21; CPython 3.13's JIT) compiles ~5 orders of magnitude cheaper than
LLVM/Cranelift — cheap enough to compile the WHOLE cold boot → run compiled from the first
call, no interpret-then-warm. Target: 175s → materially lower; foundation for AOT.

## Design
A new execution tier `cnp` (crates/rava-interp/src/cnp.rs), between the interpreter and
the Cranelift JIT. It compiles a method by emitting a straight-line native sequence — NO
per-opcode dispatch loop — cheaply (no IR, no regalloc, no optimisation).

**Stencils** = the per-opcode code fragments. Two evolutions:
1. **Subroutine-threaded (tracer bullet, low risk):** stencils are Rust `extern "C"`
   functions operating on an explicit in-memory frame (operand stack + locals). The
   compiled method is hand-emitted x64 that CALLs each stencil in sequence, frame pointer
   held in a callee-saved register, immediates passed as args. Only a tiny fixed set of
   encodings to emit (prologue, mov-imm, call, ret). Proves the pipeline end-to-end.
2. **Inlined stencils (true copy-and-patch, the real speed):** copy each stencil's
   machine-code BODY inline (no call/ret) + patch the holes (immediates, branch targets,
   next-stencil offset). Removes the per-op call overhead. Stencils captured from
   pre-compiled templates. Deferred until (1) is proven.

**Frame ABI:** `CnpFrame { stack: *mut i64, sp: usize, locals: *mut i64 }` — memory-backed
for correctness first; register-cached later.

## Phases (atomic commits)
- **P0 foundation** — exec-mem alloc (Win VirtualAlloc PAGE_EXECUTE_READWRITE) + a minimal
  x64 emitter (prologue/epilogue, mov reg,imm, call reg, ret) + CnpFrame + iconst/iload/
  iadd/ireturn stencils. Unit test: compile `iconst_2 iconst_3 iadd ireturn` → 5, execute.
- **P1 coverage** — the common straight-line opcodes (const/load/store/int+long arith/
  compare) + branches (forward/back, patch targets) + a differential test vs the
  interpreter over the bench methods and synthetic bytecode.
- **P2 integrate + MEASURE OVERHEAD** — wire cnp as tier-0: compile a method on ~first call
  (well below the Cranelift threshold), fall back to interpreter for unsupported shapes,
  Cranelift stays tier-2 for hot methods. Boot to Done, bit-exact, measure vs 175s. ← the
  user's "посмотрим оверхед".
- **P3 inline stencils + AOT + C2** — replace subroutine calls with inlined stencil bodies
  (true copy-and-patch); persist compiled code across boots (AOT cache via relocatable
  emission); keep Cranelift/(later LLVM) as tier-2 for genuine hotspots (C2 quality). ←
  the user's "AOT + C2 качество".

## Status (2026-07-01)
- **P0 ✓** foundation (exec-mem + x64 emitter + stencils), 5 tests.
- **P1 ✓** full int arithmetic + control flow (goto/if_icmp/if branches, loops), differential
  vs a reference evaluator, 5 tests.
- **P2 ✓ overhead measured, then SOLVED:** subroutine-threaded cnp was 281M op/s = 1.62x a
  dispatch loop (the per-op call/ret overhead). INLINED the stencil bodies (true
  copy-and-patch): operand stack in registers (rsi/rbx/rdi), each opcode a few inline x64
  instructions, no calls. Re-measured: **cnp 2453M op/s vs 182M op/s dispatch = 13.46x** (~180x
  vs rava's real ~13M-op/s interpreter for int code). Bonus: inline code is position-
  independent (imm32 + rel32 only) → AOT needs ZERO relocs. Validated by the full differential
  suite (all binops, all 6 icmp conditions, loops, dup/pop, AOT roundtrip).

- **P3 ✓ AOT + C2-quality mechanisms proven (tested):**
  - AOT: stencil addresses are a RELOCATION TABLE, not baked — `CompiledObj` serialises
    (to_bytes) → reloads in a fresh process (from_bytes) → `link` re-patches relocs with
    current addresses → runs. Position/ASLR independent. Proven by `aot_roundtrip` +
    `aot_rejects_corrupt`.
  - C2 quality: `c2_quality_aot_proof` compiles via Cranelift's optimising backend
    (opt_level=speed), captures the finalized bytes + relocs, runs them from our OWN
    exec page (not a JITModule). The relocs (calls to rava_* callbacks) patch at load
    exactly like the cnp relocation table → C2-quality code is AOT-capturable by the same
    mechanism.
  Remaining = productization (not a mechanism gap): wire cnp/AOT-C2 as live tiers in the
  runtime, a keyed on-disk cache, and a boot measurement.

## Honest assessment + next fork
cnp currently covers PURE INTEGER methods only — a small fraction of the boot. Two tracks to
make it matter:
- **Coverage (highest boot leverage):** getfield + array-load + a bounds-check-throw shape →
  covers the getDouble / VoxelShape.getCoords / accessor family that DOMINATES the boot
  profile. Needs the stencils to touch the VM heap (arena ptrs, GC-safe) — a real VM
  integration step, larger than the pure-int stencils.
- **P3 inline stencils:** copy stencil bodies inline (drop the per-op call) → pushes cnp
  past 1.62x toward the copy-and-patch paper's ceiling.
- **AOT + C2 quality (parity endgame):** cnp is ~2x interp, NOT C2 quality — for C2 quality
  persist CRANELIFT/LLVM output across boots (relocatable emit via cranelift-object + a
  load-time symbol-resolving loader = the AOT cache; zero warmup, C2 code). Largest build.
  Realistic combined target: cnp runs the cold-code bulk cheaply, AOT-C2 runs hot methods at
  quality with zero warmup → boot materially below 175s (object-model overhead still caps
  literal 9.7s parity; that needs a lighter object model too).

## P3 complete — all AOT+C2 mechanisms proven (2026-07-01)
Every mechanism for "AOT + C2 quality" is now implemented + tested end-to-end:
1. cnp cheap tier (compile int methods, 1.62x a dispatch loop) — P0-P2.
2. AOT relocatable serialize/load/relocate — `aot_roundtrip`.
3. C2-quality capture (Cranelift optimising backend → our own exec page) — `c2_quality_aot_proof`.
4. C2-AOT with callback relocs (Abs8 patch by symbol index) — `c2_aot_reloc_roundtrip`.
Reloc finding: is_pic=false + colocated=false ⇒ ABS8 relocs (absolute 8-byte address) —
trivial to patch at load, identical shape to cnp's stencil relocs.

## Productization plan (the live boot win — next major subsystem)
To make AOT-C2 speed up the actual boot, re-plumb the JIT compile tail (jit.rs):
- **Symbol table:** assign every rava_* callback a fixed index; expose idx↔address.
- **Relocatable compile:** the emit path already builds a Cranelift `Function`. Compile it
  via `codegen::Context::compile` (not JITModule) to get `code_buffer()` + `buffer.relocs()`;
  declare the rava_* callbacks as `UserExternalName(ns, idx)` so each reloc's target maps to
  a symbol index. Produce a serializable `{code, [(offset, sym_idx, addend)]}`.
- **Keyed disk cache:** key = (class, method, desc, bytecode blake-hash). Miss → compile +
  persist. Hit → load + relocate (patch Abs8 with current rava_* addresses) + install into
  jit_cache as `Compiled`.
- **Beat JVM:** a warm/build pass compiles ALL reachable methods → cache once; then a
  production boot loads the cache and runs compiled from the first call — zero interpret,
  zero warmup, C2 quality. This is the parity path (JVM's weakness is exactly cold-start +
  warmup). Caveat: object-model per-op overhead (field-id resolve ~1.6x, dispatch) still
  caps literal 9.7s; closing the last gap also needs a lighter object model.
- **cnp tier-0:** for methods cnp supports, use it below the Cranelift threshold (cheap) and
  AOT-cache those too; extend cnp coverage (getfield/array/invoke via the rava_* callbacks +
  a GC-safe tagged-cell frame) to reach the getDouble/getCoords accessor family that
  dominates the boot profile.

## Live-tier status (2026-07-01)
- **cnp is wired as a live tier-0** (RAVA_CNP, off by default; Windows/x86-64). The
  **pure-int-static path is boot-safe** (cnp=1 boots to Done) and a big win on compute:
  CnpBench (int loop, sub-threshold so Cranelift skips it) interp 6.4s → cnp 0.28s = **22.9x**.
  Selectivity: fire only where a loop amortises the per-call setup.
- **getfield tier (int/ref getters) — BUILT + guest-correct but REVERTED from the live
  path.** The emitter (aload/getfield/areturn + CnpCtx) is unit-tested and correct on
  guests (CnpGetter int+ref getters = 247500000), and needs cnp to `set_current_vm` so
  rava_getfield sees the VM. BUT enabling it in the full MC boot intermittently hits
  "Failed to start the minecraft server" — an arena-GC-SAFETY bug (a compiled getfield's
  receiver/reference cell can go stale under a moving collection). A `forward_if_moved`
  read-barrier on the marshalled receiver did NOT fix it, so the hazard is deeper (a GC
  during the cnp frame with un-registered cnp roots, or a stale value the interpreter
  forwards but cnp doesn't). Reverted to keep the live tier boot-safe; the emitter stays
  in git history (commit d332489) + here for a proper GC-safe redo. The RIGHT fix is to
  make the cnp frame a GC ROOT (register its stack/locals cells like the Cranelift tier's
  cells_push) so a collection during a callback forwards them — the same GC-safety the
  Cranelift tier already has. Until then cnp stays pure-int.
- Honest fit: cnp shines on COMPUTE-heavy sub-threshold methods (22.9x). The MC boot is
  invoke/dispatch-bound, so few methods are cnp-eligible there — cnp is a general
  fast-compute tier, not the MC-boot lever.

## CORRECTED DIAGNOSIS: cnp compute is correct; the boot blocker is a shared latent bug
Earlier I guessed the getfield boot failure was an arena GC-safety issue. WRONG — the
int-only path (getfield disabled) also failed once the eligibility sig was broadened. I
then TRACED what cnp fires on in the boot: 3.2M fires, almost all VoxelShape `BooleanOp`
(ZZ)Z lambdas (AND/OR/…), plus isPowerOfTwo, bit-round-up-to-pow2, RGBA pack, char-class.
Reconstructed EVERY distinct one as a differential test (cnp::tests::boolean_ops +
boot_fired_methods) — ALL match the reference. So **cnp's compute is correct on every
method it fires on in the boot.** The "Failed to start the minecraft server" is therefore a
TIMING/INTERACTION effect, not a mis-compilation — the exact same signature as the reverted
"retry" Cranelift-coverage experiment (see [[rava-dispatch-trunk]] / memory). Both = *more
methods compiled/intercepted → GC/JIT timing shifts → a latent bug elsewhere fires*.
⇒ This is a HIGH-VALUE shared lead: cnp fires on 3.2M hot BooleanOp lambdas — real boot
potential IF the latent bug is found. The next step is to capture the actual exception
behind "Failed to start" (RAVA_TRACE_MAINCATCH / RAVA_TRACE_FATAL) under broad cnp, pin the
latent bug, fix it — that unblocks BOTH cnp-on-boot AND broader Cranelift coverage. Until
then cnp stays on the safe all-int-static path (boot-safe, fires ~0 on the boot; 22.9x on
compute workloads like CnpBench).

## AOT worker boundary and field-read blocker (2026-07-31)

- The read-only `RAVA_AOT_PROBE` covered **1,785/2,000 hot methods (89.2%)** in a
  hash-exact server boot. The remaining opcode blockers were only `putstatic` (3),
  `monitorenter` (2), and `dup_x2` (1), so coverage is not the current limit.
- A full `RAVA_AOT_CLIF` boot exposed a generic execution-state violation: a cached
  AOT function could run inside a GIL-free worker even though its serial frame-root
  ABI was never admitted there. It produced a `ResourceKey.identifier` NPE in the
  worker. `Vm::aot_run` now fails closed under `parallel::gilfree()`, returning to
  the existing worker-safe JIT/interpreter route. This is a runtime-state guard,
  never a class or jar allow/deny list.
- AOT also now checks `clinit_done`, rather than `initialized`: the latter latches
  at the start of `<clinit>` and is not an execution-order proof. This still did
  not make full AOT boot-safe. A diagnostic, global `RAVA_AOT_DENY_OPS=b4` run
  reached `Done` in 37.361 s (144.2 s wall, cold compile), proving the broader
  compiled field-read/order interaction remains P0. It is deliberately **not
  shipped**: it is slower and is only a bytecode-family bisect.
- The interaction is not solely the off-GIL lane: a warm full-AOT boot with
  `RAVA_GILFREE_NOLANE=1` did not reach `Done` after 352.4 s and its isolated
  benchmark PID was stopped. Full AOT therefore remains a diagnostic path, not
  an optimization candidate, until its serial field-read/order semantics have a
  deterministic differential proof.
- Next AOT increment: make the callback-carrying field-read path observationally
  equivalent to interpretation across the serial-to-worker handoff, then remeasure
  a warm cache. Do not enable call-1 or install a workload-specific denial before
  that proof.
- Release validation of the worker guard: full `next-gen-gc` suite (416 passed,
  2 ignored), `ChunkFill_HASH=8790f04ace726ecf`, and `RingScan(8)`
  `HASH=37539c6c375d0c3`. After the `<clinit>` tightening, the same release suite
  passed again and an ordinary isolated server boot reached `Done` in 22.044 s
  (72.7 s wall).

## Verified production codegen profile (2026-07-31)

`release-fat` (`lto = "fat"`, `codegen-units = 1`) is now the verified production
build path; ordinary `release` stays thin-LTO for edit/build iteration. This is a
generic Rust backend choice, not a jar-, class-, hash-, or timing-specific runtime
branch.

Three fresh isolated P-core boots compared the two binaries with the exact same
`server.jar`, class path, environment, and restored world on every run:

| profile | Done (s) | wall (s) | outcome |
|---|---:|---:|---|
| `release` | 22.246 / 21.574 / 21.728 | 72.7 / 72.5 / 71.1 | baseline |
| `release-fat` | 19.894 / 19.295 / 19.479 | 67.6 / 67.0 / 67.4 | every pair faster |

The paired `Done` deltas were -2.352, -2.279, and -2.249 s (about -10.5%); the
ranges do not overlap. Build time was materially longer (about six minutes on this
machine), so the release profile is deliberately separate. This reduces boot time
but does not close the JVM speed/RSS objective; the next runtime increment remains
generic asynchronous JIT codegen with immortal code ownership.

## Background JIT tracer (2026-07-31)

`RAVA_BACKGROUND_JIT=1` creates one bounded, persistent compiler per VM. Planning
(field/layout interning, static-cell reservations and every inline plan) remains on
the owning VM thread; the worker receives immutable class data and emits Cranelift
code only. A completed entry is published exclusively by the main VM thread. The
worker owns its thread-local `JITModule` until `Vm` teardown, after `jit_cache` has
dropped all `JitFn`s, so no cached function can point at an unmapped module.

`force_compile` stays synchronous: if its target is already compiling, it waits for
that one bounded job and publishes it before a GIL-free lane can use the body. This
preserves the lane's compile-before-enqueue contract; an ordinary hot call simply
interprets while its pending entry is compiled. No jar, class, hash, timestamp or
stage-specific condition participates in admission.

Fresh isolated P-core boots all reached `Done`. The first pair was off 22.263 s /
72.2 s wall and on 22.910 s / 70.1 s wall. After restoring the `force_compile`
contract, a four-run alternating series was on 22.704, off 24.754, on 23.769,
off 23.252 s (`Done`); paired deltas were -2.050 and +0.517 s. The signs and bands
overlap, so it is **not promoted to the default**: it is a correctness-checked,
generic opt-in tracer, not a claimed server-boot win.

## Rejected Map.get dispatch tracer (2026-07-31)

A receiver-guarded `java/util/Map.get(Object)` callback was tested. It served only
exact native-backed JDK map classes and fell through to the original dispatch for
every custom implementation or subclass. The profile did confirm coverage: the
`Map#get` bridge disappeared and native-bridge samples fell from the earlier 16.1%
to 13.4%. That shifted work into JIT execution rather than shortening the critical
path: fresh P-core pairs were off 23.108 / on 23.223 s and on 23.362 / off 22.253 s
at `Done`. The tracer was removed rather than left as a flag or promoted.

## Rejected List interface dispatch tracer (2026-07-31)

`RAVA_LIST_INTERFACE_INTRINSIC` routed only the standard read-only
`java/util/List.size()`, `isEmpty()`, and `get(int)` sites through the already-existing
exact-`java/util/ArrayList` `$elems` serve. Any other concrete list, subclass, missing
backing array, or out-of-bounds index fell through to the unchanged general invoke; no
jar-, class-, or workload-specific condition was used.

Four fresh isolated P-core boots, restoring the same pristine world for every arm,
were `off 21.618`, `on 23.381`, `on 24.001`, `off 22.169` seconds at `Done` (walls
72.2, 75.1, 74.1, 72.9 s). Both positional `on - off` deltas were positive (+1.763 and
+1.832 s), and every `off` result beat every `on` result. The tracer was removed;
it is evidence that adding interface dispatch to this callback path shifts more work
into JIT execution without reducing the boot critical path.

## Rejected ArrayList read boundary shortcut (2026-07-31)

`RAVA_JIT_ARRAYLIST_READ_SHORTCUT` was a different route from the rejected
emit-time `List` tracer: after a compiled callback had reached `jit_call_body`, it
served only standard `Collection`/`List`/`ArrayList` `size`, `isEmpty`, and `get(int)`
through the existing exact-native-`ArrayList` `$elems` implementation. A non-exact
receiver, absent backing array, or invalid index fell through at the same boundary to
the original synthetic-frame dispatch. It had no jar-, class-, hash-, or phase-specific
admission condition.

Two fresh isolated P-core series restored the same pristine world before every boot.
The first `off,on,on,off` series reported `Done` 22.225, 23.436, 23.130, 23.234 s
(paired `on - off`: +1.211, -0.104 s); reversed `on,off,off,on` reported 23.604,
22.650, 23.314, 23.465 s (+0.954, +0.151 s). Three of four pairs favoured `off`,
and both per-series `on` minima were slower (+4.07% and +3.60%). The shortcut was
removed rather than adding a disabled check to the default hot callback path.

## Invariants
- Correctness gate every phase: differential vs the interpreter (bit-exact) + boot to Done
  byte-identical. A shape cnp can't handle yet → fall back to interpreter, never guess.
- No-adapt: generic bytecode compilation only, never keyed to a specific jar/method.
- Atomic commits; unsafe code fenced + tested.
