# Historical checkpoint/restore experiments (excluded from cold-boot acceptance)

> **Status, 2026-07-31:** checkpoint/restore is not an admissible way to meet
> Rava's `server.jar nogui` objective.  The JVM reference is a new-process cold
> boot, so Rava is measured the same way: no prior VM image, no restored heap,
> no resumed frame stack, and no launch-time checkpoint selection.  A result may
> count only when a fresh Rava process reaches `Done < 10 s` and `wall < 20 s`
> through ordinary execution.  This file is retained as a record of a separate
> generic capability; it must not be cited as performance evidence or placed on
> the cold-boot roadmap.

> This exclusion is stricter than `no-adapt`: even a jar-agnostic restore skips
> work which the JVM cold boot performs.  Snapshot code may be maintained and
> tested as an independent feature, but never used by the server benchmark,
> release gate, or optimization rationale.

## The problem, measured

Definitive full-boot A/B (2026-06-11, branch `perf/interp-speed`, all JIT wins on):

| | to "Done" | wall |
|---|---|---|
| `java -jar server.jar nogui` | **2.148 s** | ~11 s |
| `rava -jar server.jar nogui` | **3390.895 s** | ~69 min |

Rava is ~1580x slower on the cold boot.

Re-measured 2026-06-29 (branch `feat/jit-spawn-fastutil-natives`, **flat + generate-structures=false** config, RAVA_JIT_THRESHOLD=2000, same host; rava boot reaches Done only after the [[rava-jit-native-routing]] guava fix re-land):

| | MC "Done" timer | wall-to-Done |
|---|---|---|
| `java -jar server.jar nogui` (Temurin 22) | **0.205 s** | **9.26 s** |
| `rava -jar server.jar nogui` (next-gen-gc) | **15.486 s** | **~496 s** |

Cumulative wall-cracking since 2026-06-11 cut the gap from ~1580x to **~54x** wall-clock (~75x on the MC init timer). Note the two clocks: MC's `Done (Ns)` measures ONLY `DedicatedServer.initServer` (worldgen/spawn/advancements); the dominant cost — the ~8 min ServerMain class bootstrap (Blocks.<clinit>, registries, DFU codecs) — is NOT in that number. The historical proposal below was an experiment for a distinct restart feature, not a solution to the cold-boot target. The boot is **cold, one-shot bytecode** (registry build, DFU codec decode, datapack parse, worldgen) — billions of ops run once. This is the worst case for a young interpreter+JIT vs HotSpot's mature interpreter + C2.

### Why JIT cannot close it
Microbenchmarks (warm steady-state) put the Cranelift JIT at ~2x JVM on arithmetic/branchy code, ~8x on field-heavy code — and that is the *ceiling* (Cranelift is a fast-compile JIT, not an optimizing one). The boot is not warm; it is cold throughput on a huge one-shot volume. Closing 1580x on cold code requires a HotSpot-class optimizing runtime = years. **JIT is not the path.**

## Historical proposal: snapshot / restore (CRaC-style, generic)

The old proposal was to freeze a fully booted VM on a first run, then restore
it on later runs.  It is generic and jar-agnostic, but it does **not** compare
fairly with the ordinary JVM cold boot.  It is therefore explicitly out of
scope for the target above.

### Current revalidation — diagnostic only (2026-07-31)

The historical 2026-07-02 restore result and the revalidation below are not
evidence for the `wall < 20 s`, `Done < 10 s` gate.  Even when only generic
`RAVA_STACKFUL`, `RAVA_CHECKPOINT_OPS`, and `RAVA_RESTORE` mechanisms are used,
restore is excluded by the cold-boot rule.

- An 8M-op checkpoint restored heap, statics and frames in **1.824 s**, then
  resumed to `Done (22.558s)`: restore correctness is real, but this is not
  within the acceptance window.
- A frame-boundary trace found the last pre-ring checkpoint at about 51M ops.
  It is too early and leaves the compute-heavy ring to execute after restore.
- Attempts at 75M, 100M and 290M wrote **after** `Done` (approximately 4 s,
  7 s and 27 s later respectively), in the live tick loop. Restoring one of
  those files proves liveness after boot but cannot re-execute or print the
  `Done` event, so it is not a valid boot-to-Done measurement.

The remaining generic blocker is the missing checkpointable frame boundary
inside the final native-reentry/compiled segment, not a tunable checkpoint
threshold. Solving it requires resumable native continuations on the explicit
frame stack; synthesising a `Done` line, selecting a snapshot by workload, or
using a launch-time trigger would violate no-adapt and is forbidden. The target
therefore remains open on the current tree.

### The wall, and the fix
`Vm::invoke` runs a Java call chain on the **Rust call stack** (invoke → `opcodes::run` → invoke …). A live native call stack cannot be serialized, so a *running* server cannot be frozen. The fix is an **explicit `Vec<Frame>` execution machine** (trampolined): a call pushes a frame, a return pops it, and the entire execution state is plain data — serializable, hence resumable.

## Increments

| # | increment | status | commit |
|---|---|---|---|
| 0 | full-boot A/B + fix the JIT regression that broke datapack (native primitive loads → callback) | ✅ done | 3e559cd |
| 1 | `Heap::snapshot/restore` — freeze/thaw objects/arrays/maps/boxes/intern; regenerate native descriptors on restore | ✅ done | aa0f061 |
| 2 | class-statics checkpoint + `Vm::snapshot/restore_state` (heap + statics in one buffer) | ✅ done | 02b4cff |
| 3a | explicit-stack execution tracer — recursive calls on a serializable `Vec<Frame>` (integer opcode subset, synthetic program) | ✅ done | f75b4d8 |
| 3b | **checkpoint + resume live execution** — freeze `sum(10)` mid-recursion to bytes, thaw, resume → 55. The thesis, proven in code. | ✅ done | 2e3d84f |
| 3c | generalize the trampoline to the **full opcode set + real `Frame`/`Value` + constant-pool resolution**, replacing the recursion in `opcodes::run`. Flag-gated + differential-tested against the recursive interp until at parity, then default. | ⏳ **the bulk — multi-week** | — |
| 4 | extend `Vm::snapshot` to capture the real frame stack (the trampoline machine's state), not just the heap | ⏳ | — |
| 5 | native-resource re-init on restore (the dedicated-server listen socket / netty); CRaC uses before/after-checkpoint hooks | ⏳ | — |
| 6 | CLI: `--checkpoint-on-done` writes the snapshot; on start, if a snapshot exists, restore + resume instead of booting | ⏳ | — |

### 3c — current state (verified 2026-06-29)

The trampoline is **much further along than "not started."** `opcodes::run` already returns a `RunExit` enum (`Normal`/`Call`/`Yield`/`NurseryFull`/`ClinitPending`) and `drive_stack` (lib.rs ~1890) drives a serializable `Vec<(Frame, Arc<str>, Arc<str>)>`: a normal invoke stages `vm.pending` + returns `RunExit::Call`, the driver `build_frame`s + pushes the callee — NO Rust recursion. Exception unwind, GC safepoints (`NurseryFull`), and `<clinit>` flattening (`ClinitPending`) are all handled on the explicit stack. **MEASURED: `RAVA_STACKFUL=1` drives the real `server.jar` boot through block bootstrap identically to the recursive interp** (cycle 3+, 0 divergence, same RSS curve) — the execution machine handles the boot's full opcode mix.

**FIRST PARITY DIVERGENCE FOUND + PINNED (2026-06-29).** A full `RAVA_STACKFUL=1` boot drives correctly through block bootstrap → `BuiltInRegistries` → DFU datafixers → auth (`Environment[…PROD]`, ~706 s) — then **DIVERGES**: `NullPointerException: java/lang/String.substring` (caught in `net/minecraft/server/Main` → "Failed to start") where the recursive interp proceeds to `Loaded N advancements` → `Preparing level` → `Done`. `RAVA_TRACE_THROW` pinned it: thrown in **`SimpleJsonResourceReloadListener.scanDirectory`** ← `RecipeManager.prepare` ← async resource reload (`SimpleReloadInstance`→`CompletableFuture`→`WorldLoader.load`→`Main.lambda$main$1`). scanDirectory does `FileToIdConverter.listMatchingResources(rm)`→Map, then `entrySet→iterator→next→getKey→fileToId` (fileToId does the `substring`). So **a Map key/path is null under `trampoline=true` but non-null under recursion** — a NATIVE COLLECTION-ITERATION divergence in the recipe pipeline, NOT a recursion-mechanism bug (the 8 native-reentry sites all delegate to `vm.invoke`, which clears `trampoline` and runs recursively — verified, not the cause). Candidate roots: native Map/collection drain (opcodes.rs ~2577/2622) returning a null element under trampoline, `listMatchingResources` building a different map, or the CompletableFuture inline-run ordering under trampoline yielding a half-built map. Also note `if !self.trampoline { jit_direct_call }` (lib.rs ~4181) disables the JIT fast path under stackful — verify it isn't dropping a result. Next: dump scanDirectory's map + iterated keys, stackful vs recursive. ⚠️ Until parity reaches Done, `RAVA_STACKFUL` stays opt-in — the recursive boot is the only one that reaches Done.

**The remaining gap** is narrow but real: **8 native-reentry sites in `opcodes.rs`** (`invoke_virtual_native` region, ~2175/2235/2298/2469/2476/2492/2577/2622) where a NATIVE handler calls back into bytecode via `vm.invoke`/`vm.invoke_lambda`/`vm.apply` (lambda apply, synthetic-functional SAM, Collection drain → `add`, `Runnable.run`). These produce CORRECT results but recurse on the Rust stack, so a checkpoint cannot fire *while one is mid-flight* — the recursed callee frame isn't in the `drive_stack` Vec, so the captured state would be incomplete. The late `String.substring` divergence above is plausibly one of these paths handing back a different value under trampoline mode.

**Why it's still multi-week, not a quick edit:** each site sits in the MIDDLE of native dispatch logic that needs the callback's RETURN value to continue (e.g. drain elements, then keep building the collection). Trampolining them means turning native dispatch into a *resumable state machine* (stage the callback, return `RunExit::Call`, re-enter the native at a saved continuation when the callee returns) — a structural rewrite of the dispatch layer, differential-tested per the rule below. The cheaper interim (increment 4/6) is to checkpoint ONLY at frame boundaries where no native-reentry is on the stack (most bootstrap-phase boundaries qualify), accepting that a few call sites are temporarily non-checkpoint-points.

## Key design decisions (so future sessions don't re-derive)

- **Snapshot only mutable state.** Classes are *not* serialized — they reload deterministically from the jar. Freeze: heap + class statics + intern tables + (increment 4) the frame stack. Skip: native descriptors / `jit_ctx` raw pointers (regenerated), the loaded classes.
- **Re-intern on restore.** Class names are `Rc<str>`; restore re-shares one `Rc` per name so millions of objects don't each own a copy.
- **3c is differential-tested, never a blind cutover.** Run the explicit machine behind a flag and compare its result against the recursive interp on every method, exactly like `RAVA_JIT_VERIFY` does for the JIT, until zero mismatch on a full boot. The working recursive boot stays the default until then.
- **The honest risk.** Even complete, restore must deserialize a multi-GB post-Done world heap; if that approaches 2.148 s the win shrinks. Mitigations: snapshot a minimal-spawn world; memory-map / lazy-load the heap; zero-copy formats. To be measured once increment 4 lands.

## What does NOT beat the JVM (rejected)
- Snapshot *after bootstrap only* (skip the ~13 min registry/codec init via the program's own idempotency, e.g. MC's `Bootstrap.isBootstrapped`): generic and real, but worldgen (~49 min) still runs → still far slower. Only a **post-Done** snapshot + execution resume wins.
- More JIT coverage: compounds the ~2x boot speedup already banked, never reaches parity on cold code.

## 2026-07-02 session: 3c parity re-validated + two reentry sites trampolined

- **The earlier "STACKFUL reaches Done" was hollow**: with the JIT + dispatch fast paths,
  the trampoline made only ~10 boundaries the whole boot (RAVA_CKPT_DEBUG, new) — the
  bundler's Thread.start ran the ENTIRE server main inside one recursive `vm.apply` dive,
  so the machine was effectively the recursive interp (which is also why it matched it).
  RAVA_CHECKPOINT_OPS could never fire (no main-trampoline frame boundary after ~0M ops).
- **Fixed two of the 8 reentry sites** (commit ab7a347): inline-thread `start()` stages
  run() via `Vm::stage_runnable`; MethodHandle.invoke* (tail position) stages directly —
  the pending machinery lands the return on the caller's operand stack. Boundaries now
  flow through the real bootstrap (DFU/registry frames on the explicit stack).
- **New parity frontier**: `IllegalStateException: unbound value ...creative_mode_tab...`
  ~120s into a STACKFUL boot = a clinit / registry-freeze ORDERING divergence — the
  trampoline defers some <clinit> past the point the recursive machine runs it (suspect:
  the ClinitPending flattening vs a staged-call path that skips ensure_init, or the
  clinit_done gate not covering some access path). THIS is the next 3c work item.
- Trampoline round-trips cost real op-rate (a STACKFUL boot is visibly slower). Fine:
  the checkpoint boot is one-time; restores are the fast path.
- Map/stream root-gap fixes from this session (see jvm-parity.md 2026-07-02b) also fixed
  the OLD 3c divergence (String.substring NPE in scanDirectory) — it was the same
  unrooted-snapshot bug class, not a trampoline-mechanism issue as suspected.

## 2026-07-02 morning: INCREMENT 4 LANDED — freeze → restore → RESUME runs end-to-end

Chain of cracks this session (each commit-verified):
1. Map/stream root-gap fixes killed the OLD 3c divergence (String.substring NPE).
2. Thread-start + MethodHandle tail-call staged onto the trampoline → checkpoint
   boundaries flow through the whole boot (were bundler-only).
3. Sync clinits (the deferred-frame initialized-latch window closed).
4. **THE JIT×TRAMPOLINE HOLE** (fix 57dd928): Vm::invoke cleared `trampoline` only
   around the interp run — cnp_run/jit_run ran compiled code with the flag SET, so
   compiled callbacks STAGED their callee (pending) and returned without executing it
   (compiled MappedRegistry.register's holder.bindValue never ran → every holder
   unbound). With the clear moved above the compiled tiers, **RAVA_STACKFUL=1 RAVA_JIT=1
   boots to Done in 153s — recursive-machine speed, full JIT, boundaries flowing.**
5. **Increment 4 (8610c71)**: the checkpoint carries the ARENA (regions serialized WITH
   their fixed bases + class registry in id order + perm string pool + boxes) and the
   runtime classpath. MEASURED: checkpoint at 529M ops (~60% of boot) = 307 MB;
   **restore = 1.8s** (deser 483ms + re-intern 151ms + descriptors 57ms + arena);
   the restored stack RESUMES and executes the remaining boot phases for minutes.

**Current frontier (increment 5, resume-state completeness):** the resumed run dies in
RegistryDataLoader: `Unbound values in registry worldgen/noise_settings:
[large_biomes, amplified, overworld]` — these load entirely post-checkpoint from the
vanilla datapack. NOT the trampoline hole ([JIT-TRAMP]=0). VanillaPackResources
.listResources runs 4337 times on resume (listing machinery alive). Suspects, in order:
some VM/native state outside the checkpoint that the datapack registry pipeline needs —
candidates: heap static_cells interplay with re-reserved compiled statics, the
CompletableFuture/executor graph captured mid-flight, PackResources root Path natives,
green-thread state (main_thread/inline_thread_active reset to defaults on restore).
Tool: RAVA_TRACE_CALLS diff (recursive boot vs resumed run) over the RegistryDataLoader
chain to find the first diverging call.

**The math that makes this THE path:** full boot 149-153s; restore measured 1.8s. A
post-Done checkpoint (increments 5+6) ⇒ second-boot wall ≈ restore + resume-tail ≈
**seconds — under the JVM's 8.3s**. Everything else this branch shipped (149s, 18x)
is the floor for the FIRST boot only.

## Historical restore result (not a cold-boot goal): server live in 3.0s vs JVM 8.3s

Increment 5's blockers fell in sequence (each pinned empirically):
1. `Failed to parse ... from pack vanilla` on every block-state-bearing registry →
   RAVA_TRACE_THROW showed `Value false does not belong to property {persistent,[true,
   false]}` → **vm.bool_cache** (Boolean singleton cache, a Rust VM field) wasn't in the
   checkpoint: post-restore Boolean.valueOf minted FRESH objects that identity-missed the
   restored ImmutableSet property values. Same class: **vm.main_thread** (currentThread
   identity guard MinecraftServer captures). Both now checkpointed (8b5c24a).
2. With those restored: **529M-ops checkpoint resumes ALL THE WAY TO DONE** (41.5s incl.
   the remaining 40% of boot), and a **1000M-ops checkpoint ('Preparing level') resumes
   into the LIVE TICK LOOP in 2.9-3.0s** (3 runs: 2992/2969/2924 ms), pause-on-empty
   working. JVM full boot on the same host: 8310 ms. **2.8x faster than the JVM.**

Working recipe:
  freeze : RAVA_JIT=1 RAVA_STACKFUL=1 RAVA_CHECKPOINT_OPS=1000000000 \
           RAVA_CHECKPOINT_FILE=world.ckpt rava -jar server.jar nogui   (~180s, 455 MB)
  restore: RAVA_JIT=1 RAVA_RESTORE=world.ckpt rava -jar server.jar nogui  (~3.0s live)

Remaining polish (increment 6): --checkpoint-on-done ergonomics (auto-freeze at Done,
auto-restore when the file exists); zero the tick-clock on restore (the loop sees the
frozen wall-clock and logs 'Running Nms behind' then catches up — cosmetic); the Done
log line doesn't re-print on the 1000M resume (fired just past the logger call —
liveness is the tick loop + pause; a slightly earlier checkpoint would re-print it).
