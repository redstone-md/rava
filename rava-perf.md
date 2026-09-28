# RAVA Performance Notes

## 2026-06-09 getstatic JIT decision

Context:
- Branch: `perf/interp-speed`
- Base virtual-call commit: `49e8a48`
- Current tree adds generic JIT `getstatic` through the real interpreter handler, not a Minecraft-specific fast path.

Decision:
- Keep the `getstatic` patch as an uncommitted foundation for now.
- Do not commit it yet. It has not passed a fresh full `Done (...)! For help, type "help"` boot after the array-root fix, and the short interleaved GS harness does not show a clear speed win.

Validation run:
- `cargo test --release -p rava-interp`
  - Result: 10 passed, 0 failed.
- `cargo build --release -p rava-cli`
  - Result: build completed.
- Current getstatic non-verify harness smoke with `RAVA_JIT=1`:
  - `Comp`, `MathB`, `Over`, `GetF`, `Obj`, `SCall`, `VCall`, `GS`
  - Result: all exit 0, no flagged mismatch/panic/unsupported patterns.

Interleaved GS harness timings, `RAVA_JIT=1`, verify off:
- Executables:
  - baseline: `49e8a48^`
  - virtual: `49e8a48`
  - getstatic: current dirty tree
- Averages over 10 interleaved runs:
  - baseline: 825.65 ms
  - virtual: 794.72 ms
  - getstatic: 839.68 ms
- Medians over 10 interleaved runs:
  - baseline: 811.38 ms
  - virtual: 791.29 ms
  - getstatic: 805.26 ms
- Interpretation: neutral to slower on this microbench, with noisy outliers.

Correctness note:
- The getstatic callback may run class initialization, which can allocate and trigger GC while compiled frames are suspended.
- JIT cell rooting now distinguishes object handles from array handles with separate tags, so GC roots `Value::Ref` and `Value::Array` into the correct heap tables.
- Regression: `jit_cell_roots_preserve_heap_kind`.

Noise-method ceiling:
- `ImprovedNoise.gradDot` and parts of `sampleWithDerivative` contain `getstatic SimplexNoise.GRADIENT`, so getstatic can unlock some methods.
- `SimplexNoise.getValue(...)` still contains `irem`, and the JIT deliberately excludes integer div/rem opcodes, so important noise bodies still bail regardless of getstatic.
- Expected full-boot speedup from getstatic alone is therefore limited and may be erased by callback overhead.

## 2026-06-09 int div/rem JIT tracer bullet

Context:
- `SimplexNoise.getValue(...)` uses `irem`, and existing temp harnesses did not exercise hot integer remainder.
- Direct Cranelift signed div/rem is not used because it can trap on divisor zero and signed overflow; JVM semantics require `/ by zero` as `ArithmeticException`, `i32::MIN / -1 == i32::MIN`, and `i32::MIN % -1 == 0`.

Decision:
- Add generic JIT support for `idiv`/`irem` through one VM callback that mirrors the interpreter's `wrapping_div`/`wrapping_rem` behavior.
- Keep `ldiv`, `lrem`, `frem`, and `drem` excluded for separate tracer bullets.

Validation run:
- Red first: `cargo test --release -p rava-interp jit_int_ -- --nocapture`
  - Result before implementation: 3 failed at `compile(...).expect("compiled")`, proving the exclusion was the blocker.
- Green targeted: `cargo test --release -p rava-interp jit_int_ -- --nocapture`
  - Result: 3 passed.
- Full package: `cargo test --release -p rava-interp`
  - Result: 12 passed, 0 failed.
- CLI build: `cargo build --release -p rava-cli`
  - Result: build completed.

Temp `IRemB` harness, `RAVA_JIT=1`, verify off:
- JVM reference: `IREM acc=3755829230 ms=460`
- Previous `rava_getstatic.exe`: `IREM acc=3755829230 ms=24589`
- Current `target/release/rava.exe`: `IREM acc=3755829230 ms=1722`
- Interpretation: correctness matches JVM, and unblocking hot `irem` changes this microbench from interpreter fallback to compiled execution.

## 2026-06-09 reference branches and iastore

Context:
- `PerlinNoise.getValue(DDDDDZ)D` and related noise methods use `ifnull`/`ifnonnull` around nullable octave noise entries.
- Existing temp/store bytecode also uses `iastore`; adding a mutating callback required preserving fault behavior after a pending exception.

Decisions:
- Add generic JIT `ifnull`/`ifnonnull` support by comparing packed reference cells against zero.
- Add generic JIT `iastore` through a VM callback. It checks null/OOB, sets pending exceptions, and becomes a no-op after the first pending fault.
- Tighten array callbacks to require the array tag for `arraylength`, `aload`, and `iastore`.
- Fix `aload` ABI wiring by sign-extending the index to the imported callback's `i64` signature.

Validation run:
- Red first:
  - `cargo test --release -p rava-interp jit_if -- --nocapture`
  - `cargo test --release -p rava-interp jit_iastore -- --nocapture`
- Green:
  - `cargo test --release -p rava-interp`
  - Result: 16 passed, 0 failed.
- CLI build:
  - `cargo build --release -p rava-cli`
  - Result: build completed.
- Temp `IAStoreB` harness:
  - JVM: `IASTORE acc=-30237081000 last=-50331659`
  - Rava JIT: `IASTORE acc=-30237081000 last=-50331659`

Remaining noise blocker:
- Static `javap` pass shows `PerlinNoise.getValue(DDDDDZ)D` now gets past `ifnull`, but still trips the JIT's "empty operand stack at every block leader" restriction at the coordinate-selection branch.
- Next tracer bullet: support branch joins with non-empty operand stacks only when incoming stack height/types match exactly, using Cranelift block params.

## 2026-06-09 branch joins with non-empty operand stacks

Context:
- `PerlinNoise.getValue(DDDDDZ)D`, `PerlinSimplexNoise.getValue(DDZ)D`, and parts of `BlendedNoise.compute(...)` use javac patterns where values remain on the operand stack while a branch chooses another value.
- Previous JIT invariant rejected every basic-block leader reached with a non-empty operand stack, even when all incoming stack shapes matched exactly.

Decision:
- Track each block leader's operand-stack shape (`JT` list).
- Lower matching stack joins through Cranelift block parameters.
- Keep bailing out if incoming stack height/types disagree.

Validation run:
- Red first:
  - `cargo test --release -p rava-interp jit_branch_join_preserves_non_empty_operand_stack -- --nocapture`
  - Result before implementation: failed at `compile(...).expect("compiled")`.
- Green targeted:
  - Same command after implementation: 1 passed.
- Full package:
  - `cargo test --release -p rava-interp`
  - Result: 17 passed, 0 failed.
- CLI build:
  - `cargo build --release -p rava-cli`
  - Result: build completed.
- Temp `StackJoinB` harness:
  - `javap` confirmed `choose` bytecode keeps `iload_0` on stack across `ifne/goto` before `iadd`.
  - JVM: `STACKJOIN acc=13100000`
  - Rava JIT: `STACKJOIN acc=13100000`
  - Rava JIT verify: `STACKJOIN acc=13100000`, no mismatch output.

## 2026-06-09 dup2 and dastore tracer bullet

Context:
- Noise derivative/update bytecode can duplicate two-slot values and write doubles into arrays.
- `dastore` is mutating, so verify mode needs extra care: running compiled code first and then the interpreter reference path mutates the same array twice.
- A real `NoiseSmoke` harness later showed the same verify-mode problem for nested stateful calls: `XoroshiroRandomSource.nextInt()I` calls a mutating RNG method, so double-running it changes the random stream.

Decisions:
- Add generic JIT support for `dup2` for category-2 stack values.
- Add generic JIT `dastore` through a VM callback that checks null/OOB, writes the double, and no-ops after a pending exception.
- Add `has_verify_unsafe_ops(code)` and skip JIT execution in `RAVA_JIT_VERIFY=1` for methods containing callbacks or side-effecting bytecodes: array stores, field/static writes, `getstatic`, invokes, and `invokedynamic`.
- This keeps verify mode from double-applying side effects while still allowing normal JIT execution outside verify mode.

Validation run:
- Targeted:
  - `cargo test --release -p rava-interp jit_dup2_dastore_updates_double_array -- --nocapture`
  - Result: 1 passed.
  - `cargo test --release -p rava-interp jit_detects_verify_unsafe_bytecodes -- --nocapture`
  - Result: 1 passed.
- Full package after the verify guard:
  - `cargo test --release -p rava-interp`
  - Result: 19 passed, 0 failed.
- CLI build:
  - `cargo build --release -p rava-cli`
  - Result: build completed.
- Temp `DStoreB` harness:
  - JVM: `DSTORE bits=4679317906364825600,4679317459688226816,4679318370221293568`
  - Rava JIT: `DSTORE bits=4679317906364825600,4679317459688226816,4679318370221293568`
  - Rava JIT verify: `DSTORE bits=4679317906364825600,4679317459688226816,4679318370221293568`
- Temp `NoiseSmoke` harness against the real `server-1.21.11_unobfuscated.jar` noise classes (`SimplexNoise`, `ImprovedNoise`, `PerlinNoise`):
  - JVM: `NOISE accBits=9500554708150911`
  - Rava JIT: `NOISE accBits=9500554708150911`
  - Rava JIT verify after `has_verify_unsafe_ops`: `NOISE accBits=9500554708150911`
  - Before the broader guard, verify emitted repeated `XoroshiroRandomSource.nextInt()I` mismatches and produced a different accumulator, proving double-run state mutation.

Rejected experiment:
- A compile heuristic that skipped methods with invokes inside back-edge loops was tested and removed.
- It did not improve the real `NoiseSmoke` harness:
  - `perlin` 30k after the experiment: no-JIT `14.948s`, JIT `15.920s`
  - `all` 30k after the experiment: no-JIT `14.709s`, JIT `16.089s`
- Interpretation: the current slowdown is not solved by a coarse callback-in-loop filter; next instrumentation should report actual JIT compile/fail/run counts per method before another heuristic is attempted.

Next gate:
- Do not run the full Minecraft boot yet; keep adding short tracer bullets that unblock real server bytecode.
- Good next candidates: instrument JIT compile/fail counts on the `NoiseSmoke` harness to see which real noise methods still fall back, then add the smallest generic opcode/callback support needed before another expensive boot.

## 2026-06-09 static numeric helper intrinsics

Context:
- `RAVA_JIT_STATS=1` on `NoiseSmoke 30000 perlin` showed the JIT was compiling the large noise methods but still spending very hot call volume in tiny pure numeric helpers:
  - before this tracer bullet: `Mth.floor(D)I seen=480000`, `Mth.lfloor(D)J seen=360000`, `Mth.lerp(DDD)D seen=840000`, `Mth.smoothstep(D)D seen=360000`, `Mth.lerp2(DDDDDD)D seen=240000`.
- A direct owner-name intrinsic for `net/minecraft/util/Mth` would be too workload-specific.

Decision:
- Add a generic static-helper intrinsic path:
  - VM scans a method's `invokestatic` cp indices only at JIT compile time.
  - It resolves the callee through the normal method resolver.
  - The JIT inlines only exact pure bytecode shapes for `floor(D)I`, `lfloor(D)J`, `smoothstep(D)D`, `smoothstepDerivative(D)D`, `lerp(DDD)D`, `lerp2(DDDDDD)D`, and `lerp3(DDDDDDDDDDD)D`.
  - The emitter receives a `cp -> StaticIntrinsic` map, so Cranelift stays decoupled from class loading and no Minecraft owner names are embedded.
- Keep the existing `java/lang/Math` intrinsic path unchanged.
- Keep `RAVA_JIT_STATS` for now, behind the env flag. It now dumps after failed CLI runs too, and `RAVA_JIT_VERIFY=1` no longer scans bytecode when JIT is disabled.

Validation run:
- Red first:
  - `cargo test --release -p rava-interp jit_inlines_static_ -- --nocapture`
    - Result before emitter implementation: 2 failed with returned `0`, proving calls still went through the callback.
  - `cargo test --release -p rava-interp jit_detects_static_numeric_intrinsic_bytecode_shapes -- --nocapture`
    - Result before detector implementation: failed with `None`.
  - After correcting the detector to use the real `dcmpg; ifge` bytecode for `floor/lfloor`, the detector test failed once more until the matcher was updated.
- Green:
  - `cargo test --release -p rava-interp`
    - Result: 22 passed, 0 failed.
  - `cargo build --release -p rava-cli`
    - Result: build completed.
- `NoiseSmoke 30000 perlin`, `RAVA_JIT=1 RAVA_JIT_STATS=1` after this patch:
  - `NOISE accBits=4632686140208856131`
  - `Mth.floor(D)I seen=36` (down from 480000)
  - `Mth.lfloor(D)J` no longer appears in the top 60 rows (down from 360000)
  - `Mth.lerp(DDD)D seen=36`, `Mth.smoothstep(D)D seen=27`, `Mth.lerp2(DDDDDD)D seen=18`
- Timing without stats, explicit native exit code checks:
  - `NoiseSmoke 30000 perlin`: no-JIT `14.990s`, JIT `14.182s`, both `exit=0`, same `accBits=4632686140208856131`.
  - `NoiseSmoke 30000 all`: no-JIT `15.131s`, JIT `12.492s`, both `exit=0`, same `accBits=60225331441858506`.
- Verify/correctness:
  - `RAVA_JIT=1 RAVA_JIT_VERIFY=1 NoiseSmoke 20000 all`: `NOISE accBits=9500554708150911`, `exit=0`, no selected mismatch/error output.
  - `RAVA_JIT=1 RAVA_JIT_VERIFY=1 DStoreB`: `DSTORE bits=4679317906364825600,4679317459688226816,4679318370221293568`, `exit=0`.

Remaining hot callbacks:
- `ImprovedNoise.p(I)I` remains the hottest compiled method (`seen=1680000`), and it is a tiny `getfield + baload + masks` helper.
- `ImprovedNoise.gradDot(IDDD)D` and `SimplexNoise.dot([IDDD)D` remain very hot.
- Next candidate should be a generic tiny-method inliner or a cheaper callback path for small field/array accessor helpers, not another coarse skip heuristic.

## 2026-06-11 — DECISIVE A/B + direct-heap-layout roadmap

Goal (user, restated): rava FASTER than the JVM on `java -jar server.jar nogui` — fast boot, stable TPS, optimized RAM. Universal runtime; Minecraft is one workload, NOT an adaptation target.

### This session (branch perf/interp-speed, atomic commits)
- `perf(heap)` indexed maps (HashMap-backed, Java iteration order preserved)
- `feat(jar)` entry-metadata index (O(1) size/dir)
- `fix(nio)` Files.copy(InputStream,Path) dispatch + ByteArrayInputStream native (boot bundler fix)
- `feat(jit)` static-numeric-intrinsic inlining + RAVA_JIT_STATS
- `feat(jit)` reference-returning methods (areturn) — ret_jt distinguishes void vs ref; jit_run decodes via unbox_ref before truncating the box table
- `feat(jit)` if_acmpeq/if_acmpne via a rava_acmp callback that defers to the interpreter's ref_equal (now pub(crate))
All: rava-interp 25 tests pass, RAVA_JIT_VERIFY bounded boot 0 [JIT-MISMATCH], boot does not regress.

### DECISIVE A/B — callbacks are the wall (clean, interleaved)
GOTCHA: `jit_enabled = var("RAVA_JIT").is_ok()` keys on EXISTENCE — `RAVA_JIT=0` still enables JIT. UNSET for true interp.
- Compute (pure arithmetic loop, no heap): interp 28.6s → JIT 0.23s = **124x**, JVM 0.17s. JIT ≈ JVM. ✓
- ScanMap (linear identity lookup: getfield + aaload + if_acmp + areturn — the EXACT shape of a Reference2ObjectArrayMap scan): interp 21.6s → JIT **43.5s = 2x SLOWER**; JVM 0.17s (247x). The method compiled (compile_ok=1, ran native 4M×, result correct) and got WORSE.
WHY: getfield/aaload/if_acmp/arraylength are CALLBACKS (thread-local vm_ref + reconstruct &mut Vm + jit_pending check + untag + heap Vec index + Value clone + box/unbox). Per-op that EXCEEDS the interpreter's inline heap access. ScanMap pays ~3 callbacks/iter (arraylength + aaload + acmp) vs 3 inline interp ops → ~2x.
CONCLUSION: widening JIT opcode coverage (this session) is necessary for completeness but does NOT speed up the real (heap-bound) workload, and naively enabling JIT REGRESSES it. The ONLY path to beating the JVM on heap-bound code is to make field/array access NATIVE inside compiled code — no callback.

### Direct heap layout — staged design (the real lever)
Arrays are ALREADY contiguous native buffers (ArrayData::Int(Vec<i32>), Long(Vec<i64>), Double(Vec<f64>), Ref(Vec<Value>)); Java arrays are fixed-length so the Vec never reallocs → its data pointer is stable for the array's lifetime (freed only at GC, which only runs at alloc/call safepoints). Object fields are Vec<(u32,Value)> with a self-correcting slot hint. The work is the ACCESS PATH, not the storage.

Foundational ABI change (prerequisite for all stages): compiled fn signature `(locals_ptr) -> i64` → `(vm_ptr, locals_ptr) -> i64` so compiled code has the VM/heap base in a register for native loads (mirrors how a real JIT keeps the heap base live). Touches call_raw, jit_run, build_body params. Validate with verify mode + a boot.

- Stage A — native array access. Add a STABLE per-handle array descriptor table on the heap: `Vec<{data_ptr, len, elem_log2}>` indexed by handle, synced with `arrays`. The table base must never move under a live compiled frame — either pre-size to a high-water capacity or reach it through a stable indirection loaded from `vm_ptr` (NOT baked as a constant, which would go stale on grow). Emit iaload/aaload/arraylength as: load descriptor (native), native bounds compare → cold fault block calls set_pending(AIOOBE/NPE), native element load at data_ptr + idx<<elem_log2. Removes the per-element callback. EXPECTED to flip ScanMap below interp and toward JVM.
- Stage B — base/len hoisting. Without LICM, per-access still re-loads the descriptor each iteration (cheap native loads now, but ideally hoisted). Cache descriptor SSA values keyed by the JVM local holding the handle, invalidated on astore to that local / on any invoke / new (GC safepoints). Needs cross-block threading (descriptor values become loop-block params, like the operand stack already is).
- Stage C — native object fields. Give each class a fixed field-slot LAYOUT at load time (name→offset), lay objects out as a flat `Vec<i64>`/`Box<[i64]>` of tagged cells at stable offsets, and emit getfield/putfield as native load/store at the resolved offset (the slot-hint already approximates this dynamically). Then JIT wins on object-field code too.
- Stage D — inline if_acmp fast path. For the common case (both operands tagged Ref/Array/null, NOT BOX_TAG), if_acmp is a native i64 compare; only fall to the rava_acmp callback when an operand is BOX_TAG (Str/boxed-primitive, where ref_equal does content/value equality). Inline diamond: ieq = icmp eq; either_box = (x>>32==BOX_TAG_HI)|(y>>32==BOX_TAG_HI); eq = ieq if !either_box else callback. Removes the acmp callback from map-scan key compares.

Orthogonal levers (not this lever, see memory rava-perf / rava-speed-priorities): multi-threading the heap for TPS/worldgen across cores (the flagged one-core pain), and a post-bootstrap snapshot to skip the ~14min init.

Validation rule: each stage keeps the interpreter as fallback (unsupported opcode → bail → interpret) so correctness never regresses; gate every stage with RAVA_JIT_VERIFY (0 mismatch) on harnesses AND a real boot (verify masks mutating-method and ABI bugs — non-verify boot is the true gate).

## 2026-07-19 — Reference-store callback tracer bullet

Current-head evidence before editing:
- `FieldsRef` median hot section: JVM `22.4 ms`, rava `315 ms` (`14.1x`), checksum
  `779999999` on every run.
- The compiled arena reference-store path performs an inline cell store and then
  unconditionally calls `rava_card_mark`.
- Production minor GC scans the complete tenured arena. The card table is a dry-run
  verifier, and the callback's other consumers are opt-in diagnostics only.

Tracer bullet:
1. Decide at JIT compile time whether shadow-GC, shape/biome write probes, or stale-store
   tracing is enabled.
2. Emit `rava_card_mark` only when one of those diagnostics needs it.
3. Preserve the existing heap-handle/callback fallback and all diagnostic behaviour.

Gates: checksum parity, targeted reference-field tests, full release test suite,
three-run `FieldsRef` median, and a non-verify `server.jar` boot before treating the
change as production-safe.

Result:
- Default diagnostics OFF: rava median `315 ms → 183 ms` (`-41.9%`) with checksum
  unchanged; JVM median `22.4 ms`, so the gap narrows from `14.1x → 8.2x`.
- Forced `RAVA_TRACE_STALESTORE=1`: checksum still matches and the callback cost
  returns (`368 ms`), proving the compile-time diagnostic gate remains live.
- `cargo test --release -p rava-interp --features next-gen-gc`: 234 tests run,
  exit 0.
- `cargo build --release -p rava-cli --features next-gen-gc`: exit 0.
- Non-verify `server.jar` boot reached `Done (125.015s)` in `219.2 s` wall
  without a crash. The latest comparable baseline log was about `230 s` wall /
  `Done (129.626s)`; one boot is directional evidence, not a noise-proof median.
- The default-feature target already fails independently on dozens of pre-existing
  next-gen-only symbols referenced without cfg gates. The new helper itself is
  feature-gated and adds no default-target error.

## 2026-07-19 — Raw arena-reference getfield fast return

Tracer bullet:
- `native_getfield_ref_const_arena_dual` previously constructed both tagged handle
  representations even when the loaded cell was the overwhelmingly common raw
  `ArenaPtr`/null form.
- Branch on the two low tag bits first and return raw/null cells directly. Preserve
  the boxed-value callback and Ref/Array re-tag paths unchanged.

Measurement discipline:
- `FieldsRef` invokes its hot method only 16 times, so the repository's production
  threshold `512` leaves it interpreted (~29 s). The compiled A/B must explicitly use
  `RAVA_JIT=1 RAVA_JIT_THRESHOLD=10 RAVA_JIT_TIER0=1`.
- Before: seven-run median `196 ms`; after: `187/186/188 ms`, median `187 ms`
  (`-4.6%`). Checksum stayed `779999999` in every run.
- Final combined release gate: 235 tests passed under `next-gen-gc`; production CLI
  release build succeeded.

## 2026-07-20 — Generic tighter mark-sweep rearm

Production change:
- Replace the duplicated historical rearm expression with one encapsulated policy
  used by both logical heap pressure and allocator-live pressure.
- The default target is `post_live + max(post_live / 2, 128 MiB)`. The historical
  target `post_live + max(post_live, 256 MiB)` remains available through
  `RAVA_GC_REARM_TIGHT=0` or `memory.gc_rearm_tight = false` for bisection.
- `RAVA_GC_REARM_TRACE=1` / `diagnostics.gc_rearm_trace = true` is diagnostic-only;
  the default path emits no rearm output.

Server acceptance evidence:
- A tight census A/B on isolated copies reached `Done` in `146.674 s -> 136.043 s`.
  Maximum coherent RSS fell from `1532 MiB` to `1145 MiB`, and the maximum
  Rust-side census fell from `1135.8 MiB` to `880.2 MiB`.
- A clean tight run reached `Done (125.861s)` with process peak working set
  `1,275,985,920 B` (`1216.9 MiB`) and terminal working set `1,197,137,920 B`
  (`1141.7 MiB`). The prior clean historical-policy envelope was about
  `1464.9 MiB`, so the repeated peak reduction is approximately `248 MiB`.
- The array object table still crossed the 4,194,304-slot tier (`192 MiB`) in both
  census envelopes. The win therefore comes from earlier dead-slot reuse and a
  smaller transient live set; it does not claim to prevent that capacity cliff.

Generic correctness/performance gate:
- Three interleaved fresh-process `Allocs` runs per policy all produced the exact
  checksum `SUM=191999972465152`.
- Median `HOTNS` was `20.340 s` with the tighter default and `20.403 s` with the
  historical policy. This is neutral within run noise and shows no measurable
  allocation micro-regression from the additional collections.
- All six default-diagnostic runs emitted zero `[GC-REARM]` lines.

Strict no-adapt boundary:
- The policy consumes only post-collection byte counts. It receives no class,
  method, package, bytecode hash, benchmark identity or workload-specific signal.
- Minecraft remains an acceptance workload only. These measurements do not permit
  workload routing elsewhere in the runtime.

## 2026-07-20 — No-adapt spline intrinsic audit

Violation:
- The default-on CubicSpline interval intrinsic chooses execution through an exact
  Minecraft class/method/descriptor check, coordinates that route with an exact-name
  JIT compile deny, and has a second method/descriptor intercept in compiled dispatch.
  This is optimization policy, not required native/API semantics, and therefore
  violates strict no-adapt.

Fresh-copy acceptance A/B on the same `212070e` release binary:
- Intrinsic ON: `Done (127.511s)`, peak working set `1,355,649,024 B`
  (`1292.8 MiB`), terminal working set `1,255,309,312 B` (`1197.2 MiB`).
- `RAVA_NO_SPLINE_NATIVE=1`: `Done (138.072s)`, peak working set
  `1,774,948,352 B` (`1692.7 MiB`), terminal working set `1,339,867,136 B`
  (`1277.8 MiB`).
- Both isolated world copies reached `Done`. Removing the intrinsic without a
  replacement regresses the measured level-preparation time by `10.561 s` (`8.3%`),
  peak RSS by `399.9 MiB` (`30.9%`), and terminal RSS by `80.6 MiB`.

Decision:
- Do not silently delete the identity route and claim rule compliance by accepting
  a material speed/RAM regression. It remains temporary migration debt.
- Replace it with generic higher-order-call compilation: resolve the bytecode,
  inline/direct-call only from CFG/opcode/effect proofs, and keep the float-array
  search plus captured primitive predicate callback-free. Production eligibility
  must not consume class, package, method, checksum or workload identity.

Generic replacement tracer:
- `bench/BinarySearchLambda.java` reproduces only the higher-order structure: a
  primitive binary-search loop receives a captured `IntPredicate` over a sorted
  float array. It contains no workload classes or names.
- At 2,000,000 searches the JVM produced `SUM=2282806567497590031` in `92.7751 ms`;
  RAVA produced the exact same sum and semantic cases in `6.881 s` (`74.2x`).
- At 200,000 searches RAVA JIT took `683 ms` versus `2.766 s` interpreted, so the
  JIT is useful but still leaves the structural wall. JIT stats showed successful
  compilation of `hot`, the wrapper, the binary-search loop and the lambda body;
  the run nevertheless recorded 2,967,961 invoke events, including 2,755,963
  direct calls. The missing optimization is higher-order fusion/inlining, not wider
  compilation eligibility.

## 2026-07-20 — Allocation-free capturing-lambda direct packing

Implementation:
- The existing generic capturing-lambda direct path still crossed the runtime once per
  SAM call, but it also cloned the complete `Vec<Value>` capture payload and allocated
  a second `Vec<i64>` before copying into the already bounded compiled-frame buffer.
- It now borrows the live lambda captures and writes them directly into that final
  buffer. All existing fingerprint/arity/descriptor guards, ref-vs-primitive packing,
  wide-slot layout, forwarding barriers, box unwinding, GC roots and general fallback
  remain unchanged.
- `box_ref` now takes an immutable VM reference, matching its actual behaviour: flat
  refs are tagged directly and non-flat values are cloned only into the thread-local
  box table. No optimizer decision consumes an owner/class/method/workload identity.

Generic A/B (`RAVA_JIT_THRESHOLD=10`, fresh processes, exact checksums):
- 200,000 searches, five interleaved runs: median `HOTNS` fell from `692 ms` to
  `594 ms` (`-14.2%`). Every run produced `CASES=263073` and
  `SUM=-5979721553706096248`.
- 2,000,000 searches, three interleaved runs: median `HOTNS` fell from `6.930 s`
  to `6.005 s` (`-13.3%`). Every run produced `SUM=2282806567497590031`.
- JIT coverage/call counts remained identical at 200,000: all four hot bodies compiled,
  `invoke=2,967,961`, `direct=2,755,963`. The win is lower work per existing direct
  transition, not altered routing.
- Mimalloc at 200,000 reported cumulative committed memory `91.2 -> 82.9 MiB`,
  commits `650 -> 451`, faults `23,791 -> 22,362`, and peak RSS
  `76.3 -> 75.6 MiB`.

Isolated server acceptance on fresh copies of the same world/config:
- Clean `8da1f85`: `Done (173.733s)`, peak working set `1191.5 MiB`.
- Allocation-free packing: `Done (130.317s)`, peak working set `1193.3 MiB`.
- The internal boot/level-preparation time improved by `43.416 s` (`25.0%`); the
  `+1.8 MiB` peak difference is noise-sized. Both runs used the same extracted
  dependencies and received a complete post-`Preparing level` window; unstable
  external Yggdrasil waits were not used as the speed metric.

Verification:
- `cargo test --release -p rava-interp --features next-gen-gc`: 240 tests,
  238 passed, 2 ignored.
- `cargo build --release -p rava-cli --features next-gen-gc` passed.
- Disabling capturing-lambda direct dispatch retained the exact generic checksum via
  the unchanged fallback.

## 2026-07-19 — Rejected ArenaString identity tracer

## 2026-07-20 — Structural HOF fusion and CFG-proven division

Production change:
- Add default-off `RAVA_JIT_HOF_FUSION=1`. It erases an adjacent linked
  LambdaMetafactory object and inlines the exact static consumer/implementation only
  when descriptor, CFG, effect, initialization, synchronization and frame-capacity
  proofs all hold. Missing or ambiguous evidence retains normal allocation/dispatch.
- Preserve Java abrupt completion for native primitive-array faults and reject
  unsupported fault-capable implementation operations.
- Add default-off `RAVA_JIT_POSITIVE_DIV=1`. A same-local `iflt`/`ifle` fallthrough
  proof lowers nonnegative power-of-two signed division to one arithmetic shift. Every
  explicit entry into the guard/load/divisor/div sequence, switch edge, wide branch and
  legacy subroutine form fails closed.

Generic HOF acceptance, five rotated two-million-search rounds:
- Fusion on: median `HOTNS=133 ms`, wall `167.1 ms`, peak RSS `10.5 MiB`.
- Fusion off: median `HOTNS=5897 ms`, wall `6126.7 ms`, peak RSS `629.5 MiB`.
- JVM: `HOTNS=91.998 ms`, wall `153.4 ms`, peak RSS `42.5 MiB`.
- All runs produced `CASES=263073`, `SUM=2282806567497590031`, exit `0`.
- Fusion is `44.34x` faster by hot time than the unfused path and removes about
  `619 MiB` peak RSS. It is about `8.9%` behind JVM wall with about `32 MiB` less RSS.

Positive-div A/B, seven alternating pairs:
- On: median `HOTNS=128 ms`, wall `154.9 ms`, peak RSS `10.5 MiB`.
- Off: median `HOTNS=136 ms`, wall `171.2 ms`, peak RSS `10.5 MiB`.
- Every paired wall comparison favored on; exact checksum and exit were preserved.
- After closing alternate-entry soundness gaps, a three-pair release smoke retained
  `129 vs 136 ms` hot and `153.6 vs 160.1 ms` wall.
- VCode contains standalone `sarl $1` at proven sites while retaining the Java
  sign-bias sequence at unproven sites.

Rejected tracer:
- Stable primitive-array metadata behind `RAVA_JIT_HOF_ARRAY_META=1` regressed seven-run
  medians from `138 -> 142 ms` hot and `169.0 -> 171.3 ms` wall with unchanged RSS.
- The gate, metadata and tests were removed completely.

Strict no-adapt boundary:
- Production eligibility consumes only linked capabilities, exact descriptors/targets,
  bytecode, CFG, effects, initialization/synchronization state and ownership/GC proofs.
- No workload/class/method identity, package prefix, body hash, expected checksum or
  fixture-specific gate participates in routing. The Java harness and server remain
  acceptance-only.

One-core explanation:
- Guest bytecode and shared heap mutation are serialized by one global GIL.
- Daemon Java threads are cooperative by default; the optional OS daemon reacquires the
  same GIL.
- Executor, scheduler and CompletableFuture async surfaces execute `vm.apply` inline.
- Only proof-bounded `RavaPool` primitive kernels are currently genuinely GIL-free.
- The safe sequence is correct async semantics under the GIL, per-thread execution
  state, broader proven kernels, transactional detached tasks, then task-graph wiring.
  Whole-heap concurrent mutation is last, not the first shortcut.

## 2026-07-19 — Rejected ArenaString identity tracer

Hypothesis:
- Exact `String.valueOf(Object)` can return its argument unchanged when the runtime
  cell is already an ArenaString. Null, handles and non-string arena objects retained
  the existing invoke path.

Micro evidence:
- Seven interleaved threshold-10 runs preserved checksum
  `-2854891326056632319` and mixed null/non-string semantics.
- Median hot time was `4.878 s -> 0.007 s`; median process wall was
  `4.936 s -> 0.036 s`.

Rejection:
- A non-verify server boot with only the JIT tracer enabled (array trim disabled)
  reproducibly exited during vanilla datapack parsing at `86.9 s`, peak
  `1549.8 MiB`, with multiple `Failed to parse <arena-object@...>` errors.
- The optimization exposed a real Str/ArenaString compatibility dependency outside
  the focused benchmark. The full JIT patch and benchmark were removed despite the
  synthetic speedup. Microbenchmarks never override the full-boot correctness gate.

## 2026-07-19 — Inconclusive arena-major reference-array trim

Tracer:
- A default-off, generic (no workload names) arena-major STW pass trimmed only live
  reference arrays with at least 128 spare cells and more than 52% slack.
- It skipped active parallel-array regions and immediately republished the native JIT
  descriptor after every `shrink_to_fit`.
- Two targeted release tests covered descriptor pointer/length coherence, primitive
  exclusion, hysteresis and parallel-region exclusion. The combined ChunkFill run
  preserved `8790f04ace726ecf`.

Decision:
- Do not keep or commit the tracer. A completed full-boot RSS A/B could not be
  produced: clean/config and copied-world controls both reached `Preparing level`
  but not `Done` before the 600-second harness limit (peaks `1358.1 MiB` and
  `1366.8 MiB`). Running a second equally incomplete trim envelope would not prove
  a peak-memory win.
- The first rejected-JIT boot updated the root world's `level.dat`; subsequent tests
  used isolated copies and never rewrote or restored the user's world directly.
- The memory patch was removed, leaving no runtime diff.

## 2026-07-19 — Single-mutator wall and no-adapt boundary

- Production boot is predominantly single-core because guest bytecode remains behind
  one mutator/GIL. Existing multicore/fork switches in `rava.toml` are experimental
  and default off; the proven pool currently covers only statically safe kernels.
- Minecraft remains an acceptance workload, not an implementation target. New
  optimizations must be derived from generic bytecode, JVM memory and Java library
  semantics. No class-name, method-name or ChunkFill/DensityFunction special case may
  be added to make the server benchmark pass.
- The next material speed tracer must therefore be generic: either broaden
  bytecode-proven GIL-free compiled regions or make guest-thread execution safe across
  heap access, safepoints, GC roots and exception delivery. A map-key optimization is
  also valid only as complete Java `hashCode` + equals-verified collision chains,
  never as a workload-specific or hash-unique shortcut.

## 2026-07-19 — Universal multicore acceptance

The clean `next-gen-gc` release proves that host cores and the generic scheduler are
working. The remaining world-generation single-core wall is analyzer coverage plus the
shared-heap/GC contract, not missing OS permission or worker threads.

- `bench/ParReduce.java`, 80,000,000 timed iterations: JVM `84.4 ms`, RAVA serial
  kernel `119 ms`, default-on RAVA parallel kernel `18 ms`. All produced
  `SUM=9349766147328`; the generic parallel reduction was `6.6x` faster than RAVA
  serial and `4.7x` faster than the JVM. `RAVA_DBG_PAR=1` confirmed the
  `rava_parallel_reduce` fork.
- New `bench/ParallelNumeric.java`, 1,000,000 timed iterations with JIT disabled:
  `RAVA_FORK_NUMERIC` absent `437 ms`, present `61 ms`, both with
  `HASH=f4dfa96d7537fefd`. The trace reported 15 workers and a disjoint
  `[1,999999)` primitive-array scatter. Edge cases `N=0,1,2,17,100003` were
  checksum-identical OFF/ON; the 1,000,000-element checksum also matched the JVM.
- `RAVA_FORK_NUMERIC` remains default off. Its current unsafe worker contract is only
  acceptable for the narrowly proven primitive-array region; it is not permission to
  run arbitrary guest code or shared object access without the GIL.

Strict no-adapt eligibility boundary:

- Production routing must not use `RAVA_FORK_ONLY`, Minecraft/Mojang package filters,
  method names such as `compute`, `getNoiseBiome`, `findBiome`, or `initCache`, or
  benchmark hashes. Existing such branches are diagnostics/legacy experiments and
  must not be extended.
- A scheduler may consume only a bytecode/effect-derived `ParallelPlan`: iteration
  domain, rooted read-only captures, proven write partition, allocation/TLAB budget,
  ordered reduction contract, safepoint policy and serial fallback. Class, owner,
  method and workload identity must not be execution inputs.
- Arbitrary parallel mutators remain blocked on a registered-mutator epoch handshake,
  stable worker roots/exceptions, pointer reload after moving GC, race-safe publication
  and Java-memory-model-correct shared writes. Removing the GIL guard without those
  invariants would risk Rust aliasing UB and heap corruption.

## 2026-07-19 — Generic parallel rejection scan

- Rewired the existing default-off RAVA_POOL_SCAN at the JIT hot threshold. It is
  observational only: analyzer results, compilation and scheduling do not consume its
  counters or printed method identities.
- NumLoopCall proved the previously missing analyzer-ready static-call classification;
  checksum stayed stable and the scan reported one pre-inline candidate.
- Isolated server scan reached 1,500 distinct hot methods: zero callback-free MAP
  candidates, 528 coarse clean-shape rejects and 16 pre-inline-ceiling methods at the
  milestone (one later RTree distance method also appeared).
- Every printed server pre-inline site was reference-heavy collection, constructor,
  validation, builder or RTree code. None matched the proposed primitive-only static-leaf
  erasure. That runtime patch was therefore rejected before implementation: it would win
  a synthetic benchmark without improving the server acceptance workload.
- Exact names are diagnostic output only and never affect eligibility, preserving the
  strict no-adapt boundary.

## 2026-07-19 — Free-handle capacity census and rejected trim

- HeapCensus previously omitted the three free_obj/free_arr/free_map Vec allocations.
  Exact len/cap/bytes accounting now includes them in the Rust-side total.
- Isolated census measured 24.0 MiB retained capacity: object free handles peaked at
  1.02M/4.19M and array free handles at 1.23M/2.10M entries.
- A default-off hysteretic post-sweep trim tracer reclaimed about 15.3 MiB across events
  and reduced terminal retained capacity from 24.0 to 16.2 MiB.
- Full isolated OFF/ON boots both reached Done with unchanged behavior. Terminal-at-Done
  peak RSS was 1464.9 MiB OFF versus 1463.3 MiB ON, only -1.6 MiB; internal Done time was
  148.695 s versus 149.461 s. The RSS delta is noise-sized and did not justify retained
  reallocation churn.
- The trim runtime patch and its test were removed. Only the authoritative capacity
  census remains.

## 2026-07-19 — Async executor acceptance tracer

`bench/AsyncFuture.java` is the end-to-end gate for replacing inline
`CompletableFuture` execution. It uses encapsulated suppliers/concurrency probes and
asserts checksums plus peak concurrent suppliers.

Current baseline:
- JVM: blocking phase `252 ms`, peak `2`; deterministic CPU phase `11 ms`, peak `2`.
- RAVA: blocking phase `0 ms`, peak `1`; deterministic CPU phase `80 ms`, peak `1`.
- Both VMs pass checksums `29` and `12`. The zero blocking time independently proves
  that RAVA's current `Thread.sleep` native is a no-op; peak `1` proves
  `supplyAsync` is inline/serial.

Safe implementation boundary for the next tracer:
1. Resident executor queue whose `(supplier, future)` tasks are explicit GC roots.
2. Worker lifecycle registered with the VM; exactly-once completion/listener dispatch.
3. `join/get` waits on a host condition variable while releasing the GIL, then
   reacquires it before guest-state access.
4. Both mark-sweep and moving-arena forwarding visit queued/running tasks; executor
   shutdown joins workers before `Vm` destruction.
5. CPU guest bytecode remains GIL-serialized initially. GIL-free execution is only
   allowed later for already proven `WorkerContext`/offloadable suppliers.

Do not replace the inline path with detached threads before these invariants exist:
that would trade a measured performance wall for dangling guest refs, VM aliasing and
deadlocks.

## 2026-07-19 — Default-on exact capturing-lambda dedup

RAM hypothesis audit:
- Object descriptors: len `2,198,821`, capacity `4,194,304`; `30.45 MiB` slack.
- Array descriptors: len `1,968,682`, capacity `2,097,152`; `1.96 MiB` slack.
- Descriptor shrink was rejected: array savings are negligible, while shrinking the
  object table at STW GC would immediately reintroduce realloc/regrowth churn. JIT
  pointer publication is sound, but the trade is still negative.

Production change:
- Promote the already soaked arity-1 exact-key lambda cache to default-on. JLS 15.27.4
  permits reuse of an existing lambda instance. Only stable exact captures
  (Ref/Array/Int/Long/Null) participate; moving-arena `Value::Obj`, strings and FP
  values remain excluded.
- Keep the cache non-rooting and prune swept handles immediately after GC. Preserve
  `RAVA_M3_LAMBDA_DEDUP` compatibility while making its value meaningful:
  `0/false/no/off` disables, absent or true enables.

Evidence:
- Prior full-server A/B (`cc406de`) reached `Done (129.5s)`, preserved the bit-exact
  ChunkFill hash, removed 327,590 lambda objects and reduced the Rust-side census by
  `48.5 MiB`.
- New `LambdaDedup` cross-VM tracer: checksum `256991808` with both modes. Three-run
  RAVA median `5.228 s` disabled → `4.580 s` enabled (`-12.4%`) on capturing-lambda
  churn. Final default/`false` kill-switch direct runs were `4.753 s`/`5.426 s`.
- Current-head non-verify server boot reached `Done (134.586s)` without a crash:
  `286.5 s` wall, peak working set `1415.9 MiB`. The earlier audit baseline was
  `1499 MiB`, so peak is directionally `83 MiB` (`5.5%`) lower. Wall time was not an
  improvement (the run contained a long external session-key/network pause), so it
  is correctness/RAM evidence only, not a claimed end-to-end speed win.
- Final combined release gate: 235 tests passed under `next-gen-gc`; production CLI
  release build succeeded.
