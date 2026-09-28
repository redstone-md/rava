# Register-ABI arc — de-risked implementation plan

Target: collapse the per-call boundary tax (docs/jit-boundary-tax.md) that costs RAVA
11.5× on `VCall` and ~17× on MC boot. The lever: compiled methods with an all-primitive
descriptor gain a register-based ABI (`(D)D` → `fn(ctx, f64) -> f64`, args + return in
registers) instead of the uniform memory-locals ABI `fn(*mut i64 locals, i64 arr) -> i64`.

## Why sequenced, not big-bang
The call path is the most safety-critical, most-optimized code in the JIT: the 4-way
machcode PIC, the lock-free freeze-resolve table (jit.rs:1084), the G1.1 worker abort
guard, and the args buffer's double role as a **GC root region** (the collector forwards
Ref cells in `buf` in place). History (memory: repr-arc SIGSEGV, COW bypass unsound,
devirt-burn regression) shows this path punishes half-measures. Each increment below is
flag-gated (`RAVA_PRIM_ABI`, default OFF) and MUST reproduce ChunkFill hash
`8790f04ace726ecf` before the next starts.

## GC-safety scope (the key enabler)
Primitive args (`D/J/I/F/Z/B/C/S`) are NOT GC references, so passing them in registers
is GC-safe even across a callee collection. Only `L…`/`[…` (Ref) args need the
GC-visible buffer. So the arc is scoped to **all-primitive-descriptor methods** — the
receiver stays a Ref (dispatched normally), but the primitive value args + primitive
return travel in registers. `VCall.compute(D)D` and every DensityFunction node qualify.

## Increments (each: implement → build → StreamFuse/microbench → canonical 8790 → commit)

**I0 — classifier + flag (inert).** `all_primitive_desc(descriptor) -> Option<PrimSig>`
(reuse arg_slot_widths at jit.rs:8601): returns the ordered f64/i64 param kinds + return
kind, or None if any param/return is a Ref. Add `RAVA_PRIM_ABI` gate. No behavior yet.
Verify: builds clean, flag-off byte-neutral.

**I1 — register entry, invokestatic only.** RESOLVED DESIGN (impl next):

*GC-safety (the crux, now settled).* The self-reg prologue registers the frame buffer
[0..frame_len) as a GC root region; the collector forwards ONLY tagged/arena-pointer
cells and leaves primitive cells unchanged (jit.rs:15474). The memory ABI already puts
raw f64 bits of `(D)D` arg locals into that scanned buffer and it is bit-exact, so a
register entry that writes THE SAME bits into THE SAME buffer slots inherits the exact
GC contract — no new risk. Conclusion: a register entry is a buffer whose arg slots are
filled from registers instead of a caller memory-write; everything downstream is
identical.

*Scope I1a = pure primitive LEAF (no invoke/new/field/array; env.deep == None).* Such a
method has no GC point and no Ref locals, so it needs neither self-reg nor a spill buffer
— the simplest correct register entry. (Leaves are also inlined at most call sites, so
I1a validates the register-entry MECHANISM; the payoff is I2's non-leaf/virtual path,
which reuses the entry + adds the deep self-reg buffer.)

*Threading.* Avoid changing `compile_with_static_intrinsics`' signature + 4 callers: use
a thread-local `REG_ABI: RefCell<Option<RegAbi>>` set by a `compile_register()` wrapper
(single-threaded under the module lock), read at the 3 injection points.

*Injection points (measured, post-I0 line numbers):*
1. Signature (~jit.rs:9490): reg → `params = [array_desc_cell i64, <one reg per value
   param: F64/F32/I64>]`, `returns = [prim ret]`. `PrimKind::cl_type()` maps kinds.
2. Prologue — BOTH vars-init sites (jit.rs:10837 AND 11149): reg arg slots def from
   `b.block_params(entry)[1+k]` converted to the i64 cell (F64 bitcast→i64, F32
   bitcast→i32→uextend, I64 as-is); wide arg → cell at slot i, slot i+1 = 0; non-arg
   slots = 0; `locals_ptr` = a dummy internal stack slot (unused by a leaf).
3. Return arm (jit.rs:16639): reg ret F64/F32 → return the raw `sv.v`; ret I64 → `to_cell`
   (== memory path). Leaf ⇒ dv is None ⇒ no deep epilogue to reconcile.

*JitFn:* add `reg: Option<RegAbi>` + a `call_reg` for the unit test / eventual dispatch.
Verify: unit test compiles `double f(double x){…}` register-ABI, calls with an f64 arg,
asserts bit-exact vs the memory-ABI result; canonical 8790 (flag off, live path
untouched by construction — all reg code is behind `if reg.is_some()`).

**I2 — register PIC for invokevirtual/interface, all-primitive args.** Extend the 4-way
PIC hit arm: when the static descriptor is all-primitive, the cached target's register
entry is called with args in registers (receiver still loaded/untagged for the class-id
compare; only the value args skip the buffer). Miss arm unchanged (rava_invoke, generic).
This is the increment that hits `VCall` and worldgen. Verify against VCall (expect the
11.5× to fall) AND canonical 8790 AND a full MC boot A/B.

**I3 — freeze-resolve + worker path parity.** The lock-free table's direct-call arm
(jit.rs:1078) must call the register entry too, with the same arg-in-register marshalling,
under the worker abort guard. Verify: parallel worldgen (the 8790 gate under 4 workers)
stays bit-exact.

**I4 — default-on decision.** Only after I2/I3 hold bit-exact on MC boot + parallel gate
+ the corpus, and show a real boot delta, flip `RAVA_PRIM_ABI` default-on (byte-neutral
requirement waived only if the boot win is proven and the hash is still 8790).

## Reproducers (already in corpus/compute)
- `DLoop` — pure double loop (register-abi should stay ~1.29×, unaffected; no calls).
- `VCall` — virtual double tree; the I2 target (11.5× → ?). Seconds to run.
- ChunkFill 1 64 via bench/canonical_run.ps1 — the bit-exact gate (8790) + boot timing.

## Risks / open questions
- Wide params (J/D take 2 local slots): the register entry maps one register per value;
  the memory-ABI callee still uses 2 slots. The register→memory bridge (for the miss arm
  and non-primitive callees) must lay wides back into 2 slots.
- Mixed descriptors (some Ref, some primitive args) are OUT of scope for this arc — they
  keep the buffer ABI. Only all-primitive qualifies.
- Cranelift SigRef agreement: every register-ABI call_indirect needs a SigRef matching the
  target's register signature; targets are grouped by (arg-kind-vector, ret-kind) so the
  indirect call is type-consistent within a group.

## Arc conclusion (2026-07-23) — honest result

**Shipped + proven:** I0 (classifier), I1a (register entry), I1b (invokestatic register
call-site), I2a (non-leaf/instance entry) — all in-tree, flag-gated `RAVA_PRIM_ABI`
default-off, ChunkFill 8790. **StaticCall: 16750ms → 2250ms (11.7×→1.57× JVM) = 7.4×** —
the register ABI genuinely collapses the per-call tax on the static / non-inlined /
callback-heavy path where the buffer + rava_invoke cost dominates.

**I2b (virtual register PIC) — built, measured, does NOT win; reverted.** Fully
implemented (virtual resolver, 4-way inline cache keyed on the receiver class key —
arena-Obj class-id and heap-Ref class-word decoded inline — register call vs buffer
fallback). ACC bit-exact on VCall. But **VCall flag-on 24309ms vs flag-off warm-PIC
1497ms = 16× slower.** Root cause: RAVA's 4-way machcode PIC leaf-hit is already ~7.5ns;
the register path's caller spill + callee self-reg + key-scan exceed that. The deeper
truth: VCall's 11.5×-vs-JVM gap is the **call COUNT** (JVM inlines the whole tree → zero
calls), not the per-call ABI cost. Register-ABI cuts per-call cost, not call count — so it
cannot help where the warm PIC per-call cost is already tiny. Only inlining closes it, and
RAVA virtual inlining is closed (devirt-burn reverted).

**Net:** the register ABI is a real, correct capability (proven on the static path) but is
**neutral on MC boot** — MC's static all-primitive leaves are inlined (so I1b rarely fires),
and I2b can't beat the warm virtual PIC. It joins the prior neutral levers: the MC-boot wall
is inlining / call-count bound (JVM's structural advantage), not per-call-ABI bound. I3
(worker parity) and I4 (default-on) are moot — there is no MC-boot delta to justify them.

### Re-exploration (2026-07-23, second pass) — negative result confirmed

Closed the one assumption the first pass left open (that the 24s used unconditional spill and
unoptimized entries). All variants still lose to the 1.5s warm PIC on VCall:
- 1-way inline cache → 85s (polymorphic sites thrash → resolver recompiles every call);
- +process-wide compile cache +4-way inline cache +arena-Obj class-id decode → 24s (cache hits,
  ~6 resolver calls total, but each register call ~128ns/node vs the PIC's ~7.5ns);
- +conditional spill (leaf/deep low bit, spill only in the GC arm, ref stack slots threaded
  through the merge) → 25s — **the caller spill was not the bottleneck**;
- +full optimization pipeline (route the register compile through collect_inline_targets /
  build_getter_inlines / static_intrinsics / deep_shape via `with_reg_abi`, not the bare
  `compile()`) → **64s, worse**.

The register virtual path's dispatch (arena/ref key decode + 4-way scan + leaf/deep diamond,
~25-30 machcode ops) exceeds the PIC's ~10-op cid-compare + direct call, and the arg-marshal
saving doesn't compensate for tiny methods. Definitive: register-ABI cuts per-call COST, but
VCall's 11.5× gap is call COUNT (JVM inlines the tree); the register virtual PIC cannot beat
RAVA's already-tuned warm PIC. All re-exploration reverted; tree clean at I2a.

### The one untried variant (approach b) and why it still can't hit the I2 gate

The four measured variants all added a SEPARATE register dispatch (standalone
emit_virtual_reg_call) whose overhead (~25-30 machcode ops) exceeded the PIC. The one
untried variant is **approach (b): integrate into emit_dispatch_pic's hit arm** — reuse the
PIC's existing cid-compare + 4-way cache (no extra dispatch overhead) and change ONLY the
hit call from buffer to register marshal. This removes the dispatch-overhead failure mode.

But it still cannot pass the I2 gate (VCall 11.5× fall), by arithmetic: the warm-PIC leaf
hit is ~7.5ns, of which the buffer marshal + callee arg-load is only ~2-4ns. So approach (b)'s
absolute ceiling is a ~20-30% per-call trim, NOT 11.5×. VCall's 11.5×-vs-JVM gap is the CALL
COUNT (JVM inlines the whole density tree to zero calls); no register technique reduces call
count — only inlining does, and RAVA virtual inlining is closed (devirt-burn reverted). It is
not worth the deep core-PIC surgery (the most safety-critical code) for a sub-gate ~20% that
would also regress the many non-eligible virtual sites onto the slower path.

**Definitive: the register-ABI I2 payoff (collapse VCall/worldgen via the virtual PIC) is
unachievable — the gap is inlining/call-count-bound, not per-call-ABI-bound.** The arc's real,
shipped value is the static/callback boundary (I1b, StaticCall 7.4×). I3/I4 are moot: no
MC-boot delta exists to justify default-on (canonical flag-on stays 8790, SETUP/FILL within
noise — the register ABI is neutral on MC boot, joining the prior neutral levers).
