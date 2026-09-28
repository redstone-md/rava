# RIP profile of the real boot — 2026-07-27, round 2

Produced by `RAVA_PC_TRACE` (per-sample timestamped RIP trace) plus `bench/pc_trace_report.py`.
Two full boots, `release` and `release-diag`; every share below reproduces across both.
**Shares only — never quote these walls**, the runs are instrumented.

## Two method traps

1. **A RIP profile on a plain `release` build is MIS-NAMED.** `[profile.release]` carries
   `strip = "symbols"`, so its PDB holds only PUBLIC symbols and dbghelp attributes every
   private function to the nearest preceding public one. In the first run the ring's #2 host
   cost printed as `Vm::dump_jit_stats`; it is actually `Vm::fu_double_view` — a different
   function, and the one that turned out to own 10% of the ring. **Build `--profile release-diag`**
   (identical optimisation settings, symbols kept) before naming anything.
2. The sampler nominally ticks at 100 Hz but lands 75-87 samples/s (`sleep(10ms)` is a floor, and
   a dump blocks the sampler thread). Loss is uncorrelated with guest activity, so shares are
   sound; sampled-seconds columns are not wall.

## Inside compiled code, per window

% OF SAMPLES INSIDE COMPILED CODE (RIP inside a Cranelift code range), per window, both runs:

| window | wall | rip2 n | rip2 INSIDE | rip1 INSIDE |
|---|---:|---:|---:|---:|
| (a) blocks A+B | 58.2s | 4522 | **7.4%** | 6.5% |
| (b) block C | 32.0s | 2320 | **5.4%** | 5.1% |
| (c) serial residue | 2.2s | 158 | **0.6%** | 1.9% |
| (d) ring | 67.8s | 5882 | **37.0%** | 36.0% |
| whole boot | 152-165s | 13,000 | **19.3%** | 18.8% |

Two of these are corrections to the ledger, and the ring one is the big one.

1. **THE RING IS NOT "PURE COMPILED-CODE THROUGHPUT".** docs/boot-map-2026-07-26.md records the ring as "97.1% JIT_EXEC, 1.4% dispatch, 0.6% native bridges, 0% GC". That is the WALL sampler's JIT_EXEC row, and the boot map already documents why that row is misread: `rava_sampler` raises the JIT_EXEC guard around the whole of `jit_run`, so every callback out of compiled code back into Rust is billed to JIT_EXEC while no machine code runs. The correction was applied to Block B in round 1 and never applied to the ring. RIP says the ring is **37.0% inside compiled code, 63.0% host** - and the single largest host block is the dispatch host path at **29.9% self-time** (`rava_dispatch_miss` inclusive 32.4%, `jit_call` 29.6%, `jit_call_body` 29.5%). The ring is HOST-BOUND like everything else, just less so.
   This retro-explains the whole ring arc: the memo lever (hits=0) and the parallel lever (+37%) both aimed at compiled-side work that is only a third of the window, while `REF_STATIC_CELLS` - which removed a HOST CALLBACK CLUSTER - is the one that moved it -31%. Same rule, third confirmation.

2. **THE SERIAL RESIDUE HAS COLLAPSED, from 11.2s to ~2.2s.** Measured identically in both runs (2.25s / 2.26s), located by the first post-`Starting minecraft server version` sample whose stack names the ring's own work. The residue is now **3.2% of the Done window, not 11%**; the ring is **96.8%**. Consequences for the ledger:
   - The Amdahl line "`11.2 + 92.6/12 = 18.9s` on 12 P-cores, ~12s at infinite cores, so Done<=10s is unreachable IN PRINCIPLE" no longer holds on those numbers: `2.2 + 67.8/12 = 7.9s`, and ~2.2s at infinite cores. The impossibility proof is void. (Ring parallelism is still REFUTED on measurement (+37%), so this is a correction to the argument, not a new lever.)
   - "A lever on the DensityFunction/Climate object graph hits ~100% of the window rather than 11% or 89%" is now worth much less: the graph BUILD is 3%, the EVALUATION is 97%.
   - The residue window at n=158 is too small to rank - read it as a shape, not a top-15.

3. Blocks A+B at 7.4% and block C at 5.4% confirm the HOST-BOUND reframe unchanged (round 1: 3-16%, Block B 5.7%). Nothing that shipped on 2026-07-26 moved the compiled-side share; codegen still caps at well under a tenth of the pre-Done boot.

CAVEAT ON SAMPLE RATE: the sampler nominally ticks at 100 Hz but lands 75-87 samples/s (sleep(10ms) is a floor, and the 2 s dump blocks the sampler thread). Loss is not correlated with what the guest is doing, so shares are sound; sampled-seconds columns are not wall.

## (a) BLOCKS A+B - process start to `Environment:`  [58.2s wall]
4522 samples | **inside compiled code 7.4%** | host 92.6%

| % | n | leaf symbol (self-time) | dominant VERIFIED caller |
|---:|---:|---|---|
| 5.5% | 249 | `std::LocalKey::with` | `Vm::resolve_field_id` (23%) |
| 5.5% | 249 | `mi_malloc_aligned` | `RawVecInner::finish_grow` (20%) |
| 4.9% | 222 | `rava_interp::Vm::jit_direct_invoke` | `Vm::jit_direct_call` (100%) |
| 4.2% | 188 | `rava_interp::Heap::lambda_fp` | `Vm::jit_direct_probe` (100%) |
| 3.6% | 161 | `rava_interp::Heap::gc` | `Vm::gc` (98%) |
| 3.5% | 160 | `memchr` | _unprovable (IAT thunk)_ |
| 2.3% | 106 | `rava_interp::opcodes::invokevirtual` | `Vm::jit_call_body` (97%) |
| 2.0% | 91 | `rava_interp::Vm::jit_call_body` | `Vm::jit_call` (100%) |
| 2.0% | 90 | `rava_interp::Heap::get_field` | `opcodes::invokevirtual` (31%) |
| 1.6% | 71 | `rava_interp::opcodes::ref_parts` | `opcodes::invokevirtual` (74%) |
| 1.5% | 70 | `core::ptr::drop_in_place` | `opcodes::invokevirtual` (21%) |
| 1.3% | 60 | `hashbrown::HashMap::insert` | `Heap::gc` (51%) |
| 1.2% | 54 | `_NLG_Return2` (SEH unwind) | _unprovable_ |
| 1.2% | 53 | `rava_interp::natives::dispatch_owner` | `natives::try_invoke_chain` (69%) |
| 1.1% | 52 | `rava_interp::ClassLoader::super_chain` | `opcodes::invokevirtual` (73%) |

CLUSTER PARTITION (leaf self-time, sums to 100%):
| % | cluster |
|---:|---|
| 18.4% | other (long tail, no row >1%) |
| 16.8% | **dispatch host path** |
| 16.1% | interpreter opcodes / frames |
| 11.7% | allocator (mimalloc + RawVec) |
| 7.4% | compiled code |
| 6.6% | name-keyed lookup / hashing |
| 6.1% | string / substring search |
| 5.5% | thread-local access |
| 5.3% | GC / arena barriers |
| 3.1% | cranelift/regalloc COMPILE |
| 2.3% | kernel / file / zip |
| 0.6% | env / registry reads |

INCLUSIVE (function anywhere on the PROVEN chain; lower bound - the walk stops at the first indirect call and is capped at depth 6):
`Vm::jit_call_body` 29.1% | `Vm::jit_call` 28.0% | `jit::rava_dispatch_miss` 24.8% | `opcodes::invokevirtual` 15.6% | `Vm::jit_direct_call` 13.0% | `Vm::invoke` 11.8% | **`natives::dispatch_owner` 9.6%** | `LocalKey::with` 6.3% | `Heap::gc`/`Vm::gc` 6.2% | `opcodes::instance_of` 5.9%

WHAT CHANGED SINCE ROUND 1. The round-1 Block B table's top rows were `str::contains` ~9%, name-keyed lookup ~9%, allocator ~7%, dispatch ~11%. Substring search is now 6.1% and name-keyed lookup 6.6% - `owner_bits` and `fast_strhash` took the top off both but neither is gone. What has moved UP into first place is the **dispatch host path at 16.8% self / ~29% inclusive**: `jit_call_body`, `jit_direct_invoke`, `jit_direct_probe`, `Heap::lambda_fp`. The completeness critic's standing item ("the dispatch host path has NO track attacking it") is now the largest named cost in A+B, and `rava_dispatch_miss` is on the proven chain of a QUARTER of all A+B samples.
Second unowned row: `natives::dispatch_owner` at 9.6% inclusive - the native-bridge owner/name/descriptor `match` chain, entered from `try_invoke_chain`. It is a linear string-compare cascade per bridged call.
Also still present and still unmemoised: `env / registry reads` 0.6% (`RtlQueryEnvironmentVariable`/`GetEnvironmentVariableW` survive round 1's `env_memo`).

## (b) BLOCK C - `Environment:` to `Starting minecraft server version`  [32.0s wall]  ** THE PRIORITY TABLE - never RIP-profiled before **
2320 samples | **inside compiled code 5.4%** | host 94.6%

| % | n | leaf symbol (self-time) | dominant VERIFIED caller |
|---:|---:|---|---|
| 7.7% | 178 | `std::LocalKey::with` | `owner_bits::contains_any` (29%) |
| 5.2% | 120 | `memchr` | _unprovable (IAT thunk)_ |
| 4.1% | 95 | `mi_malloc_aligned` | `RawVecInner::try_allocate_in` (21%) |
| 2.6% | 61 | `rava_interp::opcodes::ref_parts` | `opcodes::invokevirtual` (70%) |
| 2.2% | 51 | `rava_interp::opcodes::invokevirtual` | `Vm::jit_call_body` (86%) |
| 2.1% | 48 | `NtCreateFile` | _unprovable (kernel)_ |
| 2.0% | 46 | `rava_interp::Vm::jit_run` | `Vm::invoke` (100%) |
| 1.9% | 43 | `rava_interp::Vm::jit_call_body` | `Vm::jit_call` (100%) |
| 1.8% | 41 | `mi_theap_realloc_aligned` | _unprovable_ |
| 1.7% | 39 | `rava_interp::Heap::get_field` | `opcodes::invokevirtual` (48%) |
| 1.6% | 37 | `_NLG_Return2` (SEH unwind) | _unprovable_ |
| 1.5% | 35 | `rava_interp::natives::dispatch_owner` | `natives::try_invoke_chain` (85%) |
| 1.5% | 35 | `core::str::next_match` | **`rava_interp::args_of` (68%)** |
| 1.4% | 33 | `rava_interp::Vm::invoke` | `Vm::invoke_lambda_snapshot` (51%) |
| 1.4% | 32 | `core::FnOnce::call_once` | `jit::rava_dispatch_miss` (6%) |

CLUSTER PARTITION:
| % | cluster |
|---:|---|
| 22.2% | other (flat tail: no row >2%) |
| 19.0% | interpreter opcodes / frames |
| 10.2% | allocator (mimalloc + RawVec) |
| 8.6% | string / substring search |
| 7.7% | thread-local access |
| 6.7% | dispatch host path |
| 6.1% | **cranelift/regalloc COMPILE** |
| 5.6% | kernel / file / zip |
| 5.4% | compiled code |
| 3.8% | name-keyed lookup / hashing |
| 3.3% | GC / arena barriers |
| 1.4% | env / registry reads |

INCLUSIVE (proven chain, lower bound):
`opcodes::invokevirtual` 24.2% | `Vm::jit_call_body` 21.8% | `Vm::invoke` 20.8% | `Vm::jit_call` 20.3% | `opcodes::run` 12.5% | `jit::rava_dispatch_miss` 11.2% | **`natives::dispatch_owner` 9.9%** | `LocalKey::with` 9.2% | **`Vm::invoke_lambda_snapshot` 8.7%** | `Vm::invoke_lambda` 5.9% | `natives::try_invoke_chain` 5.9% | `memchr` 5.2% | `stream::dispatch` 4.5% | `jit::rava_indy` 4.0%

Compiled-side (the 5.4%) is DFU codec machinery, not Minecraft logic: `Climate$RTree.lambda$comparator$3` 7.9% of compiled, then `DataResult$Success.flatMap/map/setLifecycle`, `RecordCodecBuilder$Instance$5.decode`, `ListCodec$DecoderState.accept`, `Codec$4.decode`. The 5.8s RTree build is visible but is NOT the shape of the window.

FOUR NAMED, PREVIOUSLY-UNOWNED COSTS IN BLOCK C (the ~39s of which only ~12s was attributed):

1. **`natives::dispatch_owner` - 9.9% inclusive / 1.5% self.** The native-bridge dispatcher, reached from `try_invoke_chain`. Its own source comment describes the defect ("each otherwise scans the whole nio/net/stream dispatch chain + the map-dispatch owner traversal"): a linear cascade of owner/name/descriptor string `match`es executed per bridged call. Round 1's fast-paths (Map$Entry, Optional) proved the shape; nothing hoisted the cascade itself. This is round 1's exact defect class - per-call O(string) bookkeeping - and it is the biggest named cost in the window.

2. **`Vm::invoke_lambda_snapshot` - 8.7% inclusive.** The codec/datapack decode is one long lambda chain; every `invoke_lambda` goes through a SNAPSHOT. Nobody has priced this.

3. **`LocalKey::with` - 7.7% self, the single largest leaf.** Consumers below. Block C is the window where the TLS cluster is worst.

4. **`rava_interp::args_of` - the descriptor argument-list parser - owns 68% of the `next_match` substring search** and is reached from `jit_call_body` and `jit_indy`. It re-parses a method descriptor's argument list by character search on every call that needs the arity. `args_of` on the proven chain of 1.1% of the window, but the walk stops at the first indirect call so that is a hard lower bound; the substring cluster it belongs to is 8.6%.

Two non-levers worth closing so nobody spends a track on them:
- `NtCreateFile` 2.1% + `kernel/file/zip` 5.6% total is the datapack read. Real, but small, and it is file OPENS (not reads) - not a prefetch lever.
- cranelift/regalloc COMPILE is 6.1% of block C. The JIT is still compiling here (block C compiles the codec/registry code). It is the 3rd-largest cluster and it is NOT reducible by cheapening an operation - only by compiling less. Note this cuts against any lever that would compile MORE bodies in this window.

## (c) SERIAL RESIDUE - `Starting minecraft server version` to first ring task  [2.2s wall]
158 samples | **inside compiled code 0.6%** | host 99.4%

** READ THIS FIRST: THE WINDOW IS 2.2s, NOT 11.2s. ** Measured at 2.25s (rip1) and 2.23s (rip2) - the residue->ring edge is the first post-`Starting minecraft server version` sample whose stack names `findBiome`/`Climate`, and it reproduces to within 30 ms across two independent boots. The ledger's 11.2s (and its "~3-8s" revision) is superseded: key interning plus REF_STATIC_CELLS have all but deleted this window. It is now **3.2% of the Done window**, and there is no longer a serial-residue lever worth a track.

n=158 is too small for a ranking. Every row below is 2-8 samples; treat this as a SHAPE, not a top-15.

| % | n | leaf symbol (self-time) | dominant VERIFIED caller |
|---:|---:|---|---|
| 5.1% | 8 | `std::LocalKey::with` | `Vm::resolve_method` (25%) |
| 3.8% | 6 | `mi_malloc_aligned` | `RawVecInner::try_allocate_in` (33%) |
| 3.2% | 5 | `cranelift_codegen::Verifier::run` | `verifier::verify_context` (100%) |
| 3.2% | 5 | `rava_interp::ClassLoader::super_chain` | `Vm::jit_native_served_deep` (80%) |
| 3.2% | 5 | `rava_interp::Vm::jit_run` | `Vm::invoke` (100%) |
| 3.2% | 5 | `memchr` | _unprovable_ |
| 3.2% | 5 | `rava_interp::Heap::arena_root_locations` | `Vm::arena_collect_jit` (100%) |
| 2.5% | 4 | `rava_interp::nextgen::java_string_hash` | `nextgen::tlab_alloc_string` (75%) |
| 2.5% | 4 | `rava_interp::opcodes::record_object_method` | `opcodes::invokedynamic_assemble` (100%) |
| 2.5% | 4 | `RtlQueryEnvironmentVariable` | _unprovable_ |
| 1.9% | 3 | `rava_interp::Vm::invoke` | `opcodes::invokevirtual` (33%) |
| 1.9% | 3 | `_NLG_Return2` | _unprovable_ |
| 1.9% | 3 | `rava_classfile::ClassFile::method` | `Vm::invoke` (66%) |
| 1.9% | 3 | `rava_interp::opcodes::run` | `Vm::invoke` (100%) |
| 1.3% | 2 | `core::BuildHasher::hash_one` | `std::LocalKey::with` (50%) |

CLUSTER PARTITION:
| % | cluster |
|---:|---|
| 21.5% | other |
| 17.7% | **cranelift/regalloc COMPILE** |
| 17.1% | interpreter opcodes / frames |
| 9.5% | allocator |
| 9.5% | name-keyed lookup / hashing |
| 5.7% | string / substring search |
| 5.1% | thread-local access |
| 4.4% | kernel / file / zip |
| 3.8% | GC / arena barriers |
| 2.5% | env / registry reads |
| 2.5% | dispatch host path |
| 0.6% | compiled code |

INCLUSIVE (proven chain): `Vm::invoke` 22.2% | `Vm::jit_indy` 20.9% | `jit::rava_indy` 19.0% | `Vm::value_to_string` 14.6% | `opcodes::invokedynamic_assemble` 14.6% | `opcodes::invokedynamic` 13.9% | `opcodes::record_object_method` 13.3% | `cranelift Context::compile_stencil` 10.1%

WHAT THE SHAPE SAYS. Two things, both of which argue against spending a track here:
- **~18% of the window is Cranelift compiling** (`compile_stencil` 10.1% inclusive, `optimize` 9.5%, `Verifier::run`, `regalloc2`). This is not residue work at all - it is first-touch compilation of the level-setup code paths. It cannot be removed by a residue lever.
- The residual Rava work is `jit_indy` / `invokedynamic_assemble` / `record_object_method` / `value_to_string` at ~14-21% inclusive: the record-`toString` path that key interning already fixed the expensive half of. `RandomState.create`'s stringify cost is gone from the leaf table entirely, which closes the round-1 "direction settled" item with a number.
- `RtlQueryEnvironmentVariable` at 2.5% is the highest env-read share of any window - a per-call `std::env::var_os` that round 1's `env_memo` did not cover.

## (d) RING - first ring task to `Done`  [67.8s wall, 96.8% of the Done window]
5882 samples | **inside compiled code 37.0%** | host 63.0%

| % | n | leaf symbol (self-time) | dominant VERIFIED caller |
|---:|---:|---|---|
| 17.3% | 1019 | `<jit> ImprovedNoise.sampleAndLerp(IIIDDDD)D` | _compiled, entered indirectly_ |
| 8.9% | 521 | `std::LocalKey::with` | **`Vm::resolve_field_id` (68%)** |
| 6.2% | 362 | `rava_interp::Vm::jit_direct_invoke` | `Vm::jit_direct_call` (100%) |
| 6.0% | 353 | `rava_interp::Vm::jit_direct_probe` | `Vm::jit_direct_call` (100%) |
| 5.8% | 342 | `<jit> PerlinNoise.getValue(DDDDDZ)D` | _compiled_ |
| 4.5% | 264 | `rava_interp::Vm::jit_call_body` | `Vm::jit_call` (100%) |
| 3.7% | 219 | `rava_interp::jit::rava_dispatch_miss` | _unprovable (called from compiled code)_ |
| 2.6% | 153 | `rava_interp::Vm::jit_fu_invoke` | `jit::rava_fu_invoke` (100%) |
| 2.6% | 151 | `<jit> ImprovedNoise.noise(DDDDD)D` | _compiled_ |
| 2.2% | 131 | `rava_interp::nextgen::forward_if_moved` | `Vm::jit_direct_invoke` (66%) |
| 2.0% | 116 | **`rava_interp::Vm::fu_double_view`** | `Vm::jit_fu_invoke` (100%) |
| 1.8% | 108 | `memchr` | _unprovable_ |
| 1.8% | 107 | `core::FnOnce::call_once` | `Vm::jit_direct_invoke` (28%) |
| 1.7% | 99 | `<jit> Climate$RTree$Node.distance([J)J` | _compiled_ |
| 1.4% | 81 | `rava_interp::Vm::jit_direct_call` | `Vm::jit_call_body` (100%) |

CLUSTER PARTITION:
| % | cluster |
|---:|---|
| 37.0% | compiled code |
| **29.9%** | **dispatch host path** |
| 8.9% | thread-local access |
| 8.7% | other |
| 5.7% | interpreter opcodes / frames |
| 2.5% | string / substring search |
| 2.5% | allocator |
| 2.3% | GC / arena barriers |
| 1.2% | kernel / file / zip |
| 1.0% | name-keyed lookup / hashing |
| 0.2% | env / registry reads |
| 0.02% | cranelift COMPILE (1 sample - the ring compiles nothing) |

The 37% compiled side decomposes cleanly (unlike ChunkFill FILL, which was one row):
`ImprovedNoise.sampleAndLerp` 17.3% of window / 46.8% of compiled | `PerlinNoise.getValue(DDDDDZ)D` 5.8% | `ImprovedNoise.noise` 2.6% | `Climate$RTree$Node.distance` 1.7% | `PerlinNoise.getValue(DDD)D` 1.2% | `DensityFunctions$ShiftedNoise.compute` 1.1% | `NormalNoise.getValue` 1.1% | `DensityFunctions$ShiftNoise.compute` 0.9%

INCLUSIVE (proven chain, lower bound):
`jit::rava_dispatch_miss` 32.4% | `Vm::jit_call` 29.6% | `Vm::jit_call_body` 29.5% | `Vm::jit_direct_call` 18.5% | **`jit::rava_fu_invoke` 14.0%** | `Vm::jit_fu_invoke` 13.3% | **`Vm::fu_double_view` 10.0%** | `Vm::jit_direct_invoke` 9.8% | `LocalKey::with` 9.4% | `opcodes::invokevirtual` 7.3% | `Vm::jit_direct_probe` 7.0% | **`Vm::resolve_field_id` 6.7%** | `jit::rava_direct_miss` 3.5%

=== THE FINDING: 14.0% OF THE RING IS A NAME-KEYED FIELD LOOKUP INSIDE A FAST PATH ===

One proven chain accounts for it, and it is the ONLY chain under `resolve_field_id` in the whole window (396/396 samples):

  `Vm::resolve_field_id < Vm::fu_double_view < Vm::jit_fu_invoke < jit::rava_fu_invoke`   = 6.73% of the ring

and 99.8% of the 824 fastutil-bridge samples have `PerlinNoise.getValue(DDDDDZ)D` on the stack (then `DensityFunctions$PureTransformer.compute` 30.9%, `$Ap2.compute` 10.8%, `$Mapped.transform` 9.8%).

MECHANISM, exactly: `PerlinNoise.getValue` reads `this.amplitudes.getDouble(i)` once per octave. `amplitudes` is a fastutil `DoubleImmutableList`, so the call lands in `jit_fu_invoke` -> `fu_fastpath` -> `fu_double_view` (crates/rava-interp/src/lib.rs:11799), which does, ON EVERY CALL:

    let aid = self.resolve_field_id(owner, "a");          // lib.rs:11805
    ... if is_arraylist { let sid = self.resolve_field_id(owner, "size"); }   // lib.rs:11811

`resolve_field_id` (lib.rs:19648) then goes to `tlcache::field_key_get(owner, name)` - a thread-local memo whose HIT still costs a `LocalKey::with` plus a hash of a 44-character class name and a field name. That is why `LocalKey::with` is the ring's #2 leaf at 8.9% and 68% of its proven consumers are `resolve_field_id`.

Totals: `rava_fu_invoke` 14.0% inclusive, `fu_double_view` 10.0% inclusive / 2.0% self, `resolve_field_id` 6.7% inclusive of which the TLS+hash is 6.3% of the window.

WHY IT SHOULD SHIP AND WHY IT IS SAFE UNDER THE PROJECT'S OWN RULES:
- It is a per-call host bookkeeping cost that is O(string) - the recurring defect class round 1 killed six instances of - and the fix is the same shape as the one already applied ~30 lines away: `fu_iter_field_ids` (lib.rs:11826) memoises `($elems,$pos)` in a `OnceLock` for exactly this reason. `fu_double_view` was simply never given the same treatment.
- It CHEAPENS AN OPERATION; it duplicates no body, specialises nothing, unswitches nothing. It is on the right side of the transfer rule.
- It intercepts nothing - the check is REMOVED from the hot path, not added ("interception is the expensive direction").
- Note the field ids are stable per owner but there are two owners (`DoubleImmutableList`, `DoubleArrayList`), so a bare `OnceLock` pair is not enough; a two-entry cache keyed on `is_arraylist`, or `heap.field_id_opt` cached the way `fu_iter_field_ids` does, is the shape. Field ids are interned in the heap and are not address- or handle-keyed, so `heap::HANDLE_EPOCH` / `GC_EPOCH` do NOT apply.
- Ceiling honesty: 14.0% is the INCLUSIVE cost of the whole fastutil bridge, of which roughly 8.7% (fu_double_view self + resolve_field_id subtree) is the name-keyed part. A warm host fall is ~99ns, so the callback boundary itself stays; what this removes is the TLS+hash per call, not the bridge. Budget it as "up to ~8%", A/B it, and remember this box cannot resolve an effect under ~10% on a single window without interleaved runs.

THE SECOND, BIGGER, UNOWNED COST: the dispatch host path is **29.9% of the ring self-time** and `rava_dispatch_miss` is on the proven chain of **32.4%** of ring samples. `jit_direct_probe` (6.0%) + `jit_direct_invoke` (6.2%) + `jit_direct_call` (1.4%) = 13.6% self in the direct-call cache alone, plus `jit_call_body` 4.5% and `rava_dispatch_miss` 3.7%. This is the same standing item the completeness critic raised for Block B, and in the ring it is twice as large. It is the largest single lever left anywhere on the map, and no track has ever attacked it.

`nextgen::forward_if_moved` at 2.2% (called from `jit_direct_invoke` 66% / `jit_direct_probe` 30%) is the GC forwarding barrier on the dispatch path - it is part of the same cost, not separate.