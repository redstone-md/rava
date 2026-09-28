# Rava Master Plan — beat the JVM, universally

Goal restated: `rava -jar server.jar nogui` must boot **faster than `java -jar server.jar nogui`**, hold **stable TPS**, and use **less / well-bounded RAM**. Every technique below is a *general JVM technique* (class layouts, generational GC, tiered JIT, OS threads) — server.jar is the benchmark, never the target of adaptation. No MC-specific code, ever.

## Where we are (measured, 2026-06)

| Metric | rava today | JVM | Gap |
|---|---|---|---|
| Boot to "Done" | ~3390 s (recursive interp) | ~1–2 min | ~30–60x |
| Interp throughput | ~280M bytecode ops / 60 s (~4.7M ops/s) | C2-jitted: GB-ops/s | ~100x+ per hot op |
| RAM steady | ~1.9 GB (after 21 GB→1.9 GB fix) | ~512 MB | ~4x |
| RAM transient peak | ~5 GB (worldgen burst) | ~1 GB | ~5x |
| Cores used | 1 | all | Nx |
| Known leak | `field_box`/`array_box` append-only (1.8M→2.0M+ entries during boot) | — | breaks long-run TPS |

Why the gap exists — four structural causes, in causal order:
1. **Object model.** Every object = 3 `Vec`s (`field_ids`/`field_cells`/`field_kinds`); every getfield scans ids. Per-field metadata is stored **per object** that every real JVM stores **per class**. This taxes the interpreter, blocks the JIT from emitting direct loads, triples RAM, and forces the `field_box`/`array_box` side tables (the leak) because Str doesn't fit an i64 cell.
2. **GC.** Non-moving mark-sweep, full STW, no generations. Worldgen's allocation burst (mostly short-lived) churns the free lists and produces the 5 GB transient.
3. **JIT coverage.** Cranelift JIT exists (Stage 3: invokes work) but covers a slice; ~all boot ops still run in the tree-walker.
4. **One core.** MC parallelizes worldgen (~77 % of boot) across workers; rava runs its executors inline on one thread. Even at per-op parity, 1 core cannot beat N cores on this workload — multi-core is *mandatory* for the goal, not optional.

## The plan — A → B → C → D

Ordering is by dependency, not preference: **A unblocks B, C, and D.** Doing JIT or multi-core first would build on a representation that has to be torn out anyway.

### Phase A — Class-shape object model (the foundation)
What every JVM does: field layout is a property of the **class**, computed once at class-load.
- At class load, build `Shape`: ordered (field-id, kind, offset) for the class incl. supers. Store in `LoadedClass`.
- `Object` becomes: class-id + one flat cell buffer (`Box<[i64]>` or arena slot). `field_ids`/`field_kinds` per object **die** — kinds/offsets live in the shape.
- getfield/putfield: call-site inline cache (already exists) resolves (class, name) → offset once; access = direct index. No scan.
- Strings become real heap objects (`java/lang/String` + char-array handle) instead of side-table `Value::Str` entries → **`field_box`/`array_box` die structurally; the leak is gone**, GC frees strings like anything else.
- Dynamic/reflective field writes that miss the shape: rare overflow map per object; assert-rare in heartbeat stats.
- Migration as tracer bullet: shape repr behind a flag alongside old repr, migrate opcode-by-opcode, tests + boot-to-Done green at every step; flip default, delete old repr.

**Gate A:** 34 tests + boot to Done; steady RAM ≤ 1 GB; box tables removed; interp ops/s ≥ 1.5x baseline; per-object bytes ≈ header + 8·fields.

### Phase B — Generational moving GC
Handles already give indirection → objects are movable cheaply (update handle table, not every ref).
- Young gen: per-allocation bump arena. Minor GC copies survivors to old gen; major GC = current mark-sweep over old, now compacting.
- Write barrier (card table / remembered set) for old→young refs — the one real correctness risk; mitigate with a debug "verify-heap" mode that re-scans and cross-checks the remembered set.
- Kills the 5 GB transient: burst garbage dies in the nursery and the arena is reused.

**Gate B:** peak RSS through worldgen ≤ 1.5 GB; steady ≤ 700 MB; 1-hour run shows flat memory (leak truly gone); minor-GC pause ≪ tick budget (50 ms).

### Phase C — JIT to "everything hot is compiled"
Only possible after A: fixed offsets let getfield/putfield/array ops compile to raw loads/stores.
- Tiering: invocation + loop back-edge counters (op counter exists); compile at threshold; interp stays as the always-correct fallback (per-method deopt = just don't use the JIT entry).
- Inline trivial methods (getters/setters/constructors) at call sites — biggest practical win in field-heavy code.
- Coverage driven by *profile, not names*: heartbeat reports which opcodes/methods dominate; extend codegen until ≥ 90 % of executed ops on the server.jar profile run jitted. (Profile-driven ≠ MC-specific: the codegen added is generic bytecode support.)
- Loop OSR if method-entry compilation proves insufficient for long-running loops.

**Gate C:** ≥ 90 % hot ops jitted on boot profile; boot ≤ 10 min single-core; generic microbench suite (no MC) shows ≥ C1-class throughput; tick loop runs jitted.

### Phase D — Multi-core (the decisive lever)
Composes with B: per-thread nursery arenas + shared old gen is *exactly* the sharded-heap design.
- Real OS threads for the generic JDK concurrency surface: `Thread.start`, `ExecutorService`, `ForkJoinPool`, `CompletableFuture` natives (today intercepted and run inline). Generic JDK semantics — works for any jar.
- Per-thread VM state (frame stack, JIT cells) sharing one heap; `Arc` migration already done and measured free.
- Safepoints: op-counter check parks threads for STW GC; roots = every thread's frame stack (the `exec_frames_ptr` mechanism generalizes).
- Staged: (1) threads + coarse heap `RwLock` (correctness first, parallelism only in compute), (2) per-thread nurseries from B → true parallel allocation, (3) measure, shard further only where contention shows.

**Gate D (= the goal):** boot wall-time ≤ JVM on same machine; TPS stable 20 with players; RAM within gates; all cores busy during worldgen.

Checkpoint/restore is deliberately absent from this plan.  It can be a separate
generic product feature, but it skips work that the JVM cold-boot reference
performs.  It cannot count toward `server.jar` cold-start performance, the
release gate, or this roadmap's completion criteria.

## Why this beats the JVM (honest math)
- C gives hot-path 10–50x over the tree-walker (Cranelift ≈ C1; we don't claim C2 parity per-op).
- D gives ~×cores on worldgen (77 % of boot) and parallel chunk load at runtime.
- A multiplies both (smaller objects, direct loads) and is worth ~2–4x interp + ~3x RAM on its own.
- Combined ≥ ~50–100x boot → at/under JVM wall-time through ordinary fresh-process execution.
- RAM: A+B remove the structural overhead (per-object metadata, box tables, burst retention) → 512 MB-class is reachable because the *data* estimate is already ~640 MB.

## Risk register
| Risk | Mitigation |
|---|---|
| A regresses semantics (field edge cases, reflection) | dual-repr tracer behind flag; boot-to-Done green per step; overflow map for dynamic fields |
| B write-barrier bugs (worst class of bug) | verify-heap debug mode; nursery-off flag; land after A stabilizes |
| C miscompiles | interp remains source of truth; per-method JIT disable; differential testing (run method both ways, compare) |
| D races/deadlocks | coarse-lock stage first; loom-style stress tests; thread-count=1 must equal single-thread behavior bit-for-bit |
| Scope creep toward MC | hard rule stands: only generic JDK natives/techniques; benchmark ≠ target |

## Continuous benchmark harness (build first, cheap)
One script, run per phase gate: `java` vs `rava` on server.jar — boot-to-Done wall, RSS steady/peak, post-Done TPS over 5 min — **plus** a no-MC generic suite (string/collection/numeric/alloc microbenches) proving universality. Every phase merges only if both suites pass their gate and nothing regresses.

## Sequencing summary
1. **Harness** (days) → trustworthy numbers.
2. **A — object model** (weeks; the big rewrite, highest leverage, unblocks all).
3. **B — generational GC** (weeks; needs A's layouts).
4. **C — JIT coverage** (weeks–months; needs A's offsets; biggest single-thread multiplier).
5. **D — multi-core** (months; needs B's arenas; the lever that crosses the JVM line on boot).
