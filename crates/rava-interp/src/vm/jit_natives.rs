//! Small host services a compiled body calls: strings, indy, ldc, bitsets, maps.

use crate::*;

/// Field identities of the fastutil DOUBLE list family, shared by the `&mut Vm` view and
/// its `&Vm` twin. ONE cell: the locked path resolves and fills it, and the shared path
/// only ever reads it, so the read side warms for free and can bail on a cold cell.
#[cfg(feature = "next-gen-gc")]
static DOUBLE_IDS: fu_view::FamilyIds = fu_view::FamilyIds::new();

/// The text of a string value WITHOUT copying it, for the two shapes whose text is the object's
/// own bytes: an ArenaString's inline UTF-8 and a `Value::Str`'s buffer.
///
/// `None` for everything else — a heap object carrying `$str`, a boxed primitive, null — because
/// rendering those goes through `value_to_string`, which may enter guest `toString()`. A borrow
/// must never stand in for a call that can have effects.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn borrowed_text(v: &Value) -> Option<&str> {
    match v {
        Value::Str(s) => Some(s.as_str()),
        Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
            // `value_to_string` renders this arm with `from_utf8_lossy`; a non-UTF-8 body would
            // therefore compare against replacement characters there. Refuse instead of guessing
            // — the fall-through below reproduces the old answer exactly.
            std::str::from_utf8(crate::nextgen::arena_str_bytes(*p)).ok()
        }
        _ => None,
    }
}

impl Vm {
    /// String.length()/hashCode() fallback for a NON-arena receiver (boxed Str etc.)
    /// — a thin callback (no synthetic frame). The arena case is a native load.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_str_length(&mut self, recv: i64) -> i64 {
        let v = jit::unbox_ref(self, recv, "");
        crate::natives::value_str_len(self, &v) as i64
    }
    #[cfg(feature = "next-gen-gc")]
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_str_hash(&mut self, recv: i64) -> i64 {
        let v = jit::unbox_ref(self, recv, "");
        crate::natives::value_str_hash(self, &v) as i64
    }
    /// `String.charAt(int)` fallback when the receiver is not an ASCII arena
    /// string — a thin callback (no synthetic frame). The ASCII-arena case is an
    /// inline byte load in the emitter. Byte-identical to `str_dispatch`'s
    /// "charAt" arm: surrogates unmapped, and an out-of-range index raises the
    /// same StringIndexOutOfBoundsException rather than answering U+0000.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_str_charat(&mut self, recv: i64, idx: i64) -> i64 {
        let v = jit::unbox_ref(self, recv, "");
        // The emitter routes every out-of-range index here (its inline arm takes
        // only `0 <= idx < blen`), so this is where charAt's bounds check lives
        // for compiled code. It must raise the SAME exception the interpreter's
        // `str_dispatch` arm does, or the two engines disagree on a fault.
        //
        // ⭐This used to call `value_to_string`, which returns an OWNED String — so every
        // character read allocated and copied the whole text before scanning it twice. See
        // `value_str_charat`, which borrows instead and reads an ASCII prefix in one load.
        let i = idx as i32;
        match crate::natives::value_str_charat(self, &v, i) {
            Ok(c) => c as i64,
            Err(n) => {
                jit::set_pending(
                    self,
                    "java/lang/StringIndexOutOfBoundsException",
                    &format!("Index {i} out of bounds for length {n}"),
                );
                0
            }
        }
    }

    /// `String.startsWith(String)` fallback when the receiver or prefix is not an
    /// arena string — a thin callback (no synthetic frame). The all-arena case is
    /// a native byte-compare in the emitter. Returns 1/0.
    #[cfg(feature = "next-gen-gc")]
    /// `String.startsWith(String)` fallback when the receiver or prefix is not an
    /// arena string — a thin callback (no synthetic frame). The all-arena case is
    /// a native byte-compare in the emitter. Returns 1/0.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_str_startswith(&mut self, recv: i64, prefix: i64) -> i64 {
        let rv = jit::unbox_ref(self, recv, "");
        let pv = jit::unbox_ref(self, prefix, "");
        // ⭐**A PREFIX TEST THAT COPIED THE WHOLE HAYSTACK.** `value_to_string` returns an OWNED
        // `String`, so this answered a question worth `O(len(prefix))` bytes by first mallocing
        // and memcpying `O(len(recv)) + O(len(prefix))` of them — and `len(recv)` is exactly the
        // quantity that grows: the longer the string a guest tests, the more this copies, while
        // the comparison itself gets no more expensive at all. It is the same defect the
        // `charAt` comment three functions up records, in the same file, for the same reason.
        //
        // The two shapes whose text IS the object's own bytes — an ArenaString's inline UTF-8
        // and a `Value::Str`'s buffer — are borrowed instead. Every other shape (a heap object
        // carrying `$str`, a non-string) still goes through `value_to_string`, because rendering
        // one of those can run guest `toString()` and that side effect is observable.
        if super::jit_call::fast_jitcall_enabled() {
            if let (Some(r), Some(p)) = (borrowed_text(&rv), borrowed_text(&pv)) {
                return r.starts_with(p) as i64;
            }
        }
        let (rs, ps) = match (self.value_to_string(&rv), self.value_to_string(&pv)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return 0,
        };
        if rs.starts_with(&ps) { 1 } else { 0 }
    }

    /// Allocate an ArenaString holding `s` on the nursery (collect + retry once on a
    /// full nursery, like the arena `new` path). Returns a `Value::Obj` of the string
    /// class — usable everywhere via the `str_of`/`value_to_string` duals.
    #[cfg(feature = "next-gen-gc")]
    /// Allocate an ArenaString holding `s` on the nursery (collect + retry once on a
    /// full nursery, like the arena `new` path). Returns a `Value::Obj` of the string
    /// class — usable everywhere via the `str_of`/`value_to_string` duals.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn alloc_arena_string(&mut self, s: &str) -> Value {
        let bytes = s.as_bytes();
        if let Some(p) = crate::nextgen::tlab_alloc_string(bytes) {
            return Value::Obj(p);
        }
        self.arena_collect_jit();
        match crate::nextgen::tlab_alloc_string(bytes) {
            Some(p) => Value::Obj(p),
            None => Value::Str(s.into()), // nursery can't fit it → fall back to a heap Str
        }
    }

    /// invokedynamic from COMPILED code (next-gen-gc): reconstruct the dynamic args,
    /// run the resolved CallSite (concat / lambda / …). A String-CONCAT result is
    /// promoted to an ArenaString (so the JIT's String intrinsics read it natively);
    /// other results pass through. Returns the packed result.
    #[cfg(feature = "next-gen-gc")]
    /// invokedynamic from COMPILED code (next-gen-gc): reconstruct the dynamic args,
    /// run the resolved CallSite (concat / lambda / …). A String-CONCAT result is
    /// promoted to an ArenaString (so the JIT's String intrinsics read it natively);
    /// other results pass through. Returns the packed result.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_indy(&mut self, caller_ptr: *const LoadedClass, cp: u16, raw: &[i64]) -> Result<i64, VmError> {
        let caller: Arc<LoadedClass> = unsafe {
            Arc::increment_strong_count(caller_ptr);
            Arc::from_raw(caller_ptr)
        };
        // ⚠**PRICED AND DECLINED — do not "fix" this without re-pricing it first.**
        //
        // Two per-call allocations live here: this `to_string()` of the descriptor, and the
        // unpooled `Vec<Value>` below. `jit_call_body`, the function directly beside this one,
        // already avoids BOTH — it borrows owner/descriptor out of the caller's pool and takes
        // its operand buffer from the pool — and its comment calls the two String allocations "a
        // dominant source of GC churn". So this is the same sibling-miss that produced four real
        // fixes on 2026-08-30, and it looks like an obvious fifth.
        //
        // It is not, on this workload. `RAVA_ALLOC_SITES` puts `jit_indy` at 5.1% of allocation
        // SITES in `Bootstrap.bootStrap` — the phase that is 53% of the boot — which is:
        //
        //   25 M allocations x 5.1% = 1,275,000, at 50-100 ns each
        //     = 64-128 ms of a 17,121 ms phase = **0.37-0.74% of the phase, 0.2-0.4% of the boot**
        //
        // That is under this box's measurement floor (~1% on the gate instrument, ~8% on the
        // boot), so it could be built and could not be shown to work. The rule this tree keeps
        // restating: convert to a share BEFORE writing the code.
        let desc = match caller.file.pool.invoke_dynamic(cp) {
            Some((_, _, d)) => d.to_string(),
            None => return Ok(0),
        };
        let inner = args_of(&desc);
        let bytes = inner.as_bytes();
        let mut stack: Vec<Value> = Vec::with_capacity(raw.len());
        // An indy descriptor is an explicit interpreter boundary.  Keep the same
        // descriptor-driven conversion as `jit_call_body`: only `[` parameters leave
        // the raw compiled ABI as legacy array handles.  The cache preserves alias
        // identity when the same arena array is passed in multiple array positions.
        let mut materialized_arrays: Option<Vec<(crate::arena::ArenaPtr, u32)>> = None;
        let (mut bi, mut ai) = (0usize, 0usize);
        while bi < bytes.len() && ai < raw.len() {
            let start = bi;
            let k = jit_arg_kind(bytes, &mut bi);
            if k == b'L' {
                let argument = jit::unbox_ref(self, raw[ai], &inner[start..bi.min(inner.len())]);
                stack.push(self.materialize_array_arg(
                    argument,
                    bytes.get(start) == Some(&b'['),
                    &mut materialized_arrays,
                ));
            } else {
                stack.push(jit::unpack_arg(raw[ai], k));
            }
            ai += 1;
        }
        // SYNTHETIC frame: it exists to run one opcode, has no method body, and is never handed to
        // the explicit-stack driver — so it carries no code.
        let mut frame = Frame::from_parts(std::ptr::null(), stack, 0, caller);
        frames::push(std::ptr::addr_of!(frame) as usize);
        let r = opcodes::invokedynamic(self, &mut frame, cp);
        frames::pop();
        r?;
        match frame.pop_opt() {
            // Promote a freshly built concat String onto the arena for native reads.
            Some(Value::Str(s)) => {
                let v = self.alloc_arena_string(&s);
                Ok(jit::box_ref(self, &v))
            }
            // PRIMITIVE indy result (record hashCode/equals via the ObjectMethods
            // bootstrap → int/boolean): pack FLAT. box_ref would hand the compiled
            // caller a BOX-tagged cell whose raw bits it reads as the value — a
            // garbage hash that split every TagKey-keyed map (datapack tag loading
            // failed with a ReportedException the moment such a method compiled).
            Some(v @ (Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_))) => {
                Ok(jit::pack_local(&v))
            }
            Some(v) => Ok(jit::box_ref(self, &v)),
            None => Ok(0),
        }
    }

    /// ldc of a reference constant (String/Class/…) from compiled code (next-gen-gc):
    /// resolve via the interpreter's `ldc`; a String constant is promoted to an
    /// ArenaString so the JIT's String intrinsics read it natively.
    #[cfg(feature = "next-gen-gc")]
    /// ldc of a reference constant (String/Class/…) from compiled code (next-gen-gc):
    /// resolve via the interpreter's `ldc`; a String constant is promoted to an
    /// ArenaString so the JIT's String intrinsics read it natively.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_ldc(&mut self, caller_ptr: *const LoadedClass, cp: u16) -> Result<i64, VmError> {
        let caller: Arc<LoadedClass> = unsafe {
            Arc::increment_strong_count(caller_ptr);
            Arc::from_raw(caller_ptr)
        };
        let v = self.ldc(&caller, cp)?;
        let v = match v {
            Value::Str(s) => self.alloc_arena_string(&s),
            other => other,
        };
        Ok(jit::box_ref(self, &v))
    }

    // ⚠A doc comment for a `java.util.BitSet` receiver guard used to sit here with no item under
    // it: the guard and `jit_bitset_get`/`jit_bitset_put`/`jit_bitset_isempty` moved to
    // `vm/jit_collections.rs` and the comment stayed, silently re-attaching itself to
    // `jit_fu_invoke` below and describing the wrong function to every reader and every doc
    // build. Removed rather than restated — `jit_collections.rs` carries the real one.

    /// FASTUTIL SCALAR FAST-PATH (next-gen-gc): a `getInt`/`size`/`isEmpty`/`iterator`
    /// call on an `IntImmutableList`/`IntArrayList`, served straight off the backing
    /// `int[]` with NO fastutil-bytecode synthetic frame (the swamp this kills). The
    /// JIT routes only the matching method names here (see `fu_routable`); the
    /// concrete receiver class is verified at runtime and ANYTHING unrecognised falls
    /// back to the identical general dispatch (`jit_call`), so correctness is total.
    /// `cells` = [recv, args…] exactly as the generic call buffer; the return is
    /// packed the same way `jit_call` packs it (`pack_local`/`box_ref`).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_fu_invoke(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        cells: &[i64],
    ) -> Result<i64, VmError> {
        // Read the call-site member by BORROW — the loader owns the class for the whole
        // call, so no refcount touch (the atomic inc/dec of `jit_call`'s temp-Arc is
        // pure overhead on a path taken millions of times), and no String allocation.
        // SAFETY: `caller_ptr` is a live loader-owned class for the call's duration;
        // the borrow ends with the match, before the `jit_call` fallback.
        let class: &LoadedClass = unsafe { &*caller_ptr };
        // ⭐**A SITE THAT NEVER HITS STOPS TRYING.** `fu_routable` decides admission at COMPILE
        // time from the site's declared owner, so a site typed `List` is admitted and then meets
        // guest lists, sets and views at runtime that `fu_fastpath` must refuse — and a refusal
        // pays the attempt AND the general path. That is measured: widening the iterator
        // admission by one owner made a probe 25% faster and the boot 0.7 s SLOWER, and the same
        // asymmetry blocks every remaining admission.
        //
        // `(caller_ptr, cp)` is the site's identity, so the refusals can be counted per site and
        // the hopeless ones retired. Direct-mapped, one array load and one compare on the hit
        // path; a retired site skips straight to the general call it was going to take anyway.
        // A site that ever succeeds scores positive and can never be retired.
        //
        // ⭐**AND ALL OF IT RAN BEFORE ITS OWN GATE, ON THE ENGINE'S LARGEST POPULATION.**
        // Retirement is OPT-IN — the block comment on `fu_retire_enabled` records why — and the
        // three counters below are armed only by `RAVA_DIAG`. Yet every one of the **38.0 M**
        // attempts this function's own measurement names paid, unconditionally:
        //
        //   * two site hashes (a 64-bit multiply, a shift, a mask, an xor) for a table that a
        //     default boot never reads and never writes;
        //   * `fu_site_retired` + `fu_site_note`, each opening with its own `fu_retire_enabled()`
        //     acquire load, to conclude twice that retirement is off;
        //   * `fu_served()` / `fu_missed()`, each a `OnceLock` acquire load before `inc()` can
        //     discover the counter is disarmed.
        //
        // Four acquire loads and two hashes per attempt, ~152 M loads a boot, all answering
        // questions fixed before `main` ran. What grows is the ATTEMPT COUNT, and none of this
        // work is a function of the call — it is a function of the environment.
        //
        // `fu_call_diag()` is the OR of exactly those four gates, cached in one cell. False (the
        // default) skips the block entirely; true runs the original sequence unchanged, so an
        // armed diagnostic still sees every event it used to. Kill switch `RAVA_NO_FAST_JITCALL`.
        let diag = fu_call_diag();
        let (site, site_key) = if diag {
            let site = fu_site_slot(caller_ptr as usize, cp);
            let site_key = fu_site_key(caller_ptr as usize, cp);
            if fu_site_retired(site, site_key) {
                crate::diag::fu_site_skipped().inc();
                return self.jit_call(caller_ptr, cp, kind, cells);
            }
            (site, site_key)
        } else {
            (0, 0)
        };
        let handled = match class.file.pool.member_ref(cp) {
            Some(m) => self.fu_fastpath(m.name, m.descriptor, cells)?,
            None => None,
        };
        // ⭐**THE LARGEST POPULATION IN THE ENGINE, AND NOBODY HAD SPLIT IT.** `fu_fastpath` runs
        // **38.0 M times a boot** at 65.3 ns self time = **7.53% of the wall** — four and a half
        // times the crossing count, and it is supposed to be the CHEAPEST service level. Whether
        // that 7.53% is work or waste turns entirely on this line: a hit answers the call from
        // machine code, a miss has paid the whole attempt and still owes the general path.
        if diag {
            if handled.is_some() {
                crate::diag::fu_served().inc();
            } else {
                crate::diag::fu_missed().inc();
                super::fu_miss_census::note(self, caller_ptr as usize, cp, cells);
            }
            fu_site_note(site, site_key, handled.is_some());
        }
        match handled {
            Some(r) => Ok(r),
            None => {
                // ★ A FAST-PATH MISS IS NOT A NATIVE CALL. `fu_invoke` is emitted for sites whose
                // DECLARED owner is a recognised list interface, but the runtime receiver may be
                // an ordinary guest class implementing it — and then this path fell straight into
                // `jit_call`, i.e. the full native gauntlet (`owner_bits`, `super_chain`,
                // `try_invoke_chain`, six stages) for a plain compiled-to-compiled call.
                //
                // Measured (ledger row 198): 7,880,965 fast-path refusals in one boot, of which
                // **7,876,425 (99.94%) are `net/minecraft/world/phys/shapes/CubePointRange`** —
                // a real MC class with real bytecode, reached through the `DoubleList` interface
                // by the voxel-shape code that dominates block-state construction. Trying the
                // direct-call path first serves exactly those, and a miss still falls through to
                // the unchanged gauntlet, so nothing that used to be served stops being served.
                if let Some(r) = self.jit_direct_call(caller_ptr, cp, kind, cells) {
                    return r;
                }
                self.jit_call(caller_ptr, cp, kind, cells)
            }
        }
    }

    /// A fastutil list class's field identity, from the per-class memo when
    /// `RAVA_FU_FIELD_IDS` is on, else resolved by name exactly as before.
    ///
    /// The identity never changes, but re-deriving it costs a thread-local access
    /// plus a hash of a 44-character class name PER CALL — 6.73% of the ring window,
    /// and the only chain under `resolve_field_id` in it (see `crate::fu_view`).
    /// Hoisting it out is a pure removal: no per-call check is added (the gate is a
    /// process-constant `OnceLock` read, the same one `iter_intrinsic_enabled`
    /// already makes on this path), no body is duplicated, nothing is specialised.
    #[cfg(feature = "next-gen-gc")]
    /// A fastutil list class's field identity, from the per-class memo when
    /// `RAVA_FU_FIELD_IDS` is on, else resolved by name exactly as before.
    ///
    /// The identity never changes, but re-deriving it costs a thread-local access
    /// plus a hash of a 44-character class name PER CALL — 6.73% of the ring window,
    /// and the only chain under `resolve_field_id` in it (see `crate::fu_view`).
    /// Hoisting it out is a pure removal: no per-call check is added (the gate is a
    /// process-constant `OnceLock` read, the same one `iter_intrinsic_enabled`
    /// already makes on this path), no body is duplicated, nothing is specialised.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fu_field_id(&mut self, cell: &fu_view::FieldIdCell, owner: &str, name: &str) -> u32 {
        // ⭐**THREE ACQUIRE LOADS TO SERVE ONE CACHED `u32`.** The memo hit below costs
        // `memo_enabled()`, then `cell.get()`, then — inside `note_memo_hit` — `probe_enabled()`,
        // and two of those three answer a question the environment fixed before `main` ran. The
        // callers make it worse by multiplication: `fu_int_view` / `fu_double_view` ask for TWO
        // ids on an array-list receiver, so an attempt out of `fu_fastpath`'s 38.0 M pays six.
        //
        // `fu_memo_fast()` is the production configuration proved once — memo on, probe off — so
        // a hit becomes the cell read and nothing else. The probe being off is exactly what makes
        // dropping `note_memo_hit`/`note_name_lookup` sound here: both are no-ops in that arm.
        // Any other configuration (probe armed, memo disabled, kill switch) falls to the original
        // body below, unchanged.
        if fu_memo_fast() {
            if let Some(id) = cell.get() {
                return id;
            }
            let id = self.resolve_field_id(owner, name);
            cell.set(id);
            return id;
        }
        let memo = fu_view::memo_enabled();
        if memo {
            if let Some(id) = cell.get() {
                fu_view::note_memo_hit();
                return id;
            }
        }
        let id = self.resolve_field_id(owner, name);
        fu_view::note_name_lookup();
        if memo {
            cell.set(id);
        }
        id
    }

    /// `(backing int[] handle, logical length)` of a fastutil int list. The backing
    /// array field is always `a`; `IntArrayList`'s logical length is its `size` field
    /// (the array is over-allocated), while an immutable list spans the whole array.
    #[cfg(feature = "next-gen-gc")]
    /// `(backing int[] handle, logical length)` of a fastutil int list. The backing
    /// array field is always `a`; `IntArrayList`'s logical length is its `size` field
    /// (the array is over-allocated), while an immutable list spans the whole array.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fu_int_view(&mut self, recv: &Value, is_arraylist: bool) -> Option<(u32, usize)> {
        static IDS: fu_view::FamilyIds = fu_view::FamilyIds::new();
        let owner = if is_arraylist {
            "it/unimi/dsi/fastutil/ints/IntArrayList"
        } else {
            "it/unimi/dsi/fastutil/ints/IntImmutableList"
        };
        let aid = self.fu_field_id(IDS.a(is_arraylist), owner, "a");
        let Value::Array(h) = self.read_value_field(recv, aid)? else {
            return None;
        };
        let phys = self.heap.array_len(h)?;
        let len = if is_arraylist {
            let sid = self.fu_field_id(IDS.size(), owner, "size");
            // ⭐**AN `IntArrayList` WAS PUBLISHING ITS FIELD IDS INTO THE `DoubleArrayList`
            // CONSTANTS, AND FIRST WRITER WINS FOREVER.**
            //
            // `crate::jit::DALIST_A_FID` / `DALIST_SIZE_FID` have exactly one contract, stated at
            // their definition in `jit/runtime/ic.rs`: the interner ids of
            // `it/unimi/dsi/fastutil/doubles/DoubleArrayList`'s `a` and `size`. Three emitted arms
            // BAKE them as immediates — `emit::collections`'s inline `getDouble`, and the
            // `getDouble` / `size` shapes in `emit::iterator_inline` — each behind a guard that
            // has already proven the receiver's class word IS `DoubleArrayList`.
            //
            // This block published the ids of `it/unimi/dsi/fastutil/ints/IntArrayList` into them.
            // `resolve_field_id` interns the COMPOSITE `Owner#name` for a declared field, so
            // `IntArrayList#a` and `DoubleArrayList#a` are different ids by construction — this
            // was not a near miss, it was a different field of a different class. The block is a
            // verbatim paste of the double-list publisher (its own comment still tells the
            // double-list story about `$elems`) into the INT view.
            //
            // WHY IT IS PERMANENT. Every writer of these cells — this one, the correct one in
            // `fu_double_view_shared`, and the compile-time one in `Vm::jit_run` — is
            // first-writer-wins (`if load() == 0`). `IntArrayList` is built constantly while
            // Minecraft assembles its block-state table, so it reached here first and locked the
            // real publisher out for the whole process.
            //
            // WHAT IT COST. `dalist_ready()` then reads TRUE with the wrong ids, so the three arms
            // are EMITTED. On a genuine `DoubleArrayList` receiver the class-word guard passes,
            // and the arm then reads a field id that is not in this receiver's layout: the slot
            // probe misses, `native_getfield_ref` takes its cold arm, the `is_arr` test fails, and
            // the site jumps to `slow` — i.e. the whole `rava_fu_invoke` callback it was built to
            // avoid, with a failed guard chain and a field-read callback in front of it. Strictly
            // worse than never emitting the arm, and invisible: the emitted guard cannot answer
            // wrongly, it can only never hit. Same failure the `getDouble` arm's own comment
            // records having already been burned by once, when it read the iterator's `$elems`.
            //
            // IS: publish nothing from the int view. The double list's ids are published by the
            // two places that hold a `DoubleArrayList` — `fu_double_view_shared` (the path 10.4 M
            // `getDouble` crossings take) and `Vm::jit_run`'s compile-time resolve — and with this
            // cell no longer stolen, the earlier of those two wins as designed.
            //
            // Also removes two relaxed atomic loads from every `IntArrayList` view. They are not
            // free even when both stores are long done: an atomic load is an optimisation barrier,
            // so LLVM may not sink the `size` read across them.
            //
            // `RAVA_NO_FAST_JITNAT=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) puts the old
            // publisher back verbatim, so one binary A/Bs both arms.
            #[cfg(feature = "next-gen-gc")]
            if !fast_jitnat_enabled() {
                use std::sync::atomic::Ordering::Relaxed;
                if crate::jit::DALIST_A_FID.load(Relaxed) == 0 && aid != 0 {
                    crate::jit::DALIST_A_FID.store(aid as i64 + 1, Relaxed);
                }
                if crate::jit::DALIST_SIZE_FID.load(Relaxed) == 0 && sid != 0 {
                    crate::jit::DALIST_SIZE_FID.store(sid as i64 + 1, Relaxed);
                }
            }
            match self.read_value_field(recv, sid) {
                Some(Value::Int(n)) if n >= 0 => (n as usize).min(phys),
                _ => phys,
            }
        } else {
            phys
        };
        Some((h, len))
    }

    /// `(backing double[] handle, logical length)` of a fastutil double list — the
    /// double-precision mirror of `fu_int_view`. Backing field is always `a`;
    /// `DoubleArrayList`'s logical length is its over-allocated `size` field, while a
    /// `DoubleImmutableList` spans the whole array.
    ///
    /// THE RING'S NAMED HOT SPOT: `PerlinNoise.getValue` reads
    /// `this.amplitudes.getDouble(i)` once per octave off a `DoubleImmutableList`, so
    /// this function runs once per octave per noise sample. 99.8% of the ring's
    /// fastutil-bridge samples have that method on the stack.
    #[cfg(feature = "next-gen-gc")]
    /// `(backing double[] handle, logical length)` of a fastutil double list — the
    /// double-precision mirror of `fu_int_view`. Backing field is always `a`;
    /// `DoubleArrayList`'s logical length is its over-allocated `size` field, while a
    /// `DoubleImmutableList` spans the whole array.
    ///
    /// THE RING'S NAMED HOT SPOT: `PerlinNoise.getValue` reads
    /// `this.amplitudes.getDouble(i)` once per octave off a `DoubleImmutableList`, so
    /// this function runs once per octave per noise sample. 99.8% of the ring's
    /// fastutil-bridge samples have that method on the stack.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fu_double_view(&mut self, recv: &Value, is_arraylist: bool) -> Option<(u32, usize)> {
        let IDS = &DOUBLE_IDS;
        let owner = if is_arraylist {
            "it/unimi/dsi/fastutil/doubles/DoubleArrayList"
        } else {
            "it/unimi/dsi/fastutil/doubles/DoubleImmutableList"
        };
        let aid = self.fu_field_id(IDS.a(is_arraylist), owner, "a");
        let Value::Array(h) = self.read_value_field(recv, aid)? else {
            return None;
        };
        let phys = self.heap.array_len(h)?;
        let len = if is_arraylist {
            let sid = self.fu_field_id(IDS.size(), owner, "size");
            match self.read_value_field(recv, sid) {
                Some(Value::Int(n)) if n >= 0 => (n as usize).min(phys),
                _ => phys,
            }
        } else {
            phys
        };
        Some((h, len))
    }

    /// Cached `($elems id, $pos id)` for the native iterator's synthetic fields, so the
    /// hot `hasNext`/`nextInt` read/write by id (no name hash). Resolvable once any
    /// iterator exists (those fields are interned by `iterator_of`); until then → None.
    #[cfg(feature = "next-gen-gc")]
    /// Cached `($elems id, $pos id)` for the native iterator's synthetic fields, so the
    /// hot `hasNext`/`nextInt` read/write by id (no name hash). Resolvable once any
    /// iterator exists (those fields are interned by `iterator_of`); until then → None.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fu_iter_field_ids(&self) -> Option<(u32, u32)> {
        static IDS: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
        if let Some(v) = IDS.get() {
            return Some(*v);
        }
        let elems = self.heap.field_id_opt("$elems")?;
        let pos = self.heap.field_id_opt("$pos")?;
        let _ = IDS.set((elems, pos));
        Some((elems, pos))
    }

    /// Is this receiver one of OUR native-backed maps? A `$map` field holding the table id is
    /// the shape every map arm in `map_method` already requires, so testing it is the same
    /// question the general path asks — asked one dispatch earlier. Resolved by INTERNED id, so
    /// the test is a field read, not a name hash.
    pub(crate) fn fu_map_id(&self, h: u32) -> Option<u32> {
        static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let fid = match ID.get() {
            Some(v) => *v,
            None => {
                let v = self.heap.field_id_opt("$map")?;
                let _ = ID.set(v);
                v
            }
        };
        match self.heap.get_field_hint(h, fid, 0).0 {
            Value::Int(id) if id >= 0 => Some(id as u32),
            _ => None,
        }
    }

    /// SHARED twin of `read_value_field` — `&Vm`, so its signature proves it only reads.
    ///
    /// Every operation it performs was already `&self` (`get_field_hint`, `arena_getfield`);
    /// the `&mut` on the original is inherited from its neighbours, not required by it.
    pub(crate) fn read_value_field_shared(&self, recv: &Value, id: u32) -> Option<Value> {
        match recv {
            Value::Ref(h) => Some(self.heap.get_field_hint(*h, id, 0).0),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => Some(crate::opcodes::arena_getfield(*p, id)),
            _ => None,
        }
    }

    /// SHARED twin of `fu_double_view`, for a `DoubleImmutableList` only.
    ///
    /// Returns `None` on a COLD field-id memo rather than resolving it: resolution needs
    /// `&mut Vm`, and the whole point of this path is not to need one. A cold call falls
    /// through to the locked path, which warms the memo for everyone after it.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fu_double_view_shared(&self, recv: &Value, is_arraylist: bool) -> Option<(u32, usize)> {
        if !fu_view::memo_enabled() {
            return None;
        }
        // The SAME memo the locked path warms. Declaring a private one here would have
        // produced a cell nothing ever fills, i.e. a fast path that silently never fires.
        let aid = DOUBLE_IDS.a(is_arraylist).get()?;
        let Value::Array(h) = self.read_value_field_shared(recv, aid)? else {
            return None;
        };
        let phys = self.heap.array_len(h)?;
        // A `DoubleArrayList`'s array is over-allocated, so its LOGICAL length is the
        // `size` field; the immutable flavour spans the whole array. Same rule as the
        // exclusive `fu_double_view` — read from the same memo, never re-resolved here.
        let len = if is_arraylist {
            let sid = DOUBLE_IDS.size().get()?;
            // ⭐**PUBLISH FOR THE COMPILED ARM FROM *THIS* TWIN, NOT THE `&mut` ONE.** The inline
            // `getDouble` arm needs `a` and `size` by id, and the first attempt published them
            // from `fu_double_view` — which this workload never calls. 10.4 M `getDouble`
            // crossings all arrive here, at the SHARED reader. A publisher on the path nobody
            // takes is the same defect as the arm that read `$elems`: correct-looking, and
            // never reached. +1 so 0 keeps meaning "unknown".
            //
            // ⭐**AND IT IS A ONE-TIME PUBLICATION READ TWICE PER CALL, FOREVER.** Both cells are
            // written together and both `aid`/`sid` come from an already-warm memo here (this
            // function returns `None` on a cold cell rather than resolving), so after the first
            // `DoubleArrayList` view the two loads can only ever confirm what the previous call
            // confirmed — on the path this file's own comment bills at **10.4 M crossings a
            // boot**. Short-circuited on `a` alone: `size` is written in the same breath here and
            // in `Vm::jit_run`'s twin, and the one writer that can set them apart (`jit_run`
            // guards each store on its own `!= 0`) leaves `size` zero, which the `||` still
            // catches. Steady state is therefore ONE relaxed load instead of two — and the load
            // that goes is an optimisation barrier, not just a `mov`: with it there LLVM may not
            // sink the `size` field read below it.
            //
            // Same shape as the test `Vm::jit_run` already spells for these two cells.
            //
            // ⚠**DELIBERATELY NOT BEHIND `RAVA_NO_FAST_JITNAT`, AND THE REASON IS THE POINT.**
            // The outer test guards a body that still re-tests each cell independently, so the
            // set of stores, their order and their values are the ones the old code produced,
            // for every possible state of the two cells — this is a load count, not a behaviour,
            // and there is nothing for a switch to withdraw. Gating it would also be
            // self-defeating: `fast_jitnat_enabled()` is a `OnceLock` ACQUIRE load, i.e. dearer
            // than the relaxed load it would be protecting, so the guarded form would be slower
            // than the form it replaces. A kill switch that costs more than its subject is how a
            // gate becomes the tax it was added to remove.
            use std::sync::atomic::Ordering::Relaxed;
            if crate::jit::DALIST_A_FID.load(Relaxed) == 0
                || crate::jit::DALIST_SIZE_FID.load(Relaxed) == 0
            {
                if crate::jit::DALIST_A_FID.load(Relaxed) == 0 {
                    crate::jit::DALIST_A_FID.store(aid as i64 + 1, Relaxed);
                }
                if crate::jit::DALIST_SIZE_FID.load(Relaxed) == 0 {
                    crate::jit::DALIST_SIZE_FID.store(sid as i64 + 1, Relaxed);
                }
            }
            match self.read_value_field_shared(recv, sid) {
                Some(Value::Int(n)) if n >= 0 => (n as usize).min(phys),
                _ => phys,
            }
        } else {
            phys
        };
        Some((h, len))
    }

    /// Read an instance field by id across the heap-handle / arena-object split.
    /// Read an instance field by id across the heap-handle / arena-object split.
    pub(crate) fn read_value_field(&mut self, recv: &Value, id: u32) -> Option<Value> {
        match recv {
            Value::Ref(h) => Some(self.heap.get_field_hint(*h, id, 0).0),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => Some(crate::opcodes::arena_getfield(*p, id)),
            _ => None,
        }
    }

    /// Write a field by id on a receiver that may be a heap `Value::Ref` OR an
    /// arena `Value::Obj` (the dual of `read_value_field`). Returns false if the
    /// receiver is neither. Used by the field-updater / atomic natives so a CAS on
    /// an arena-allocated object updates the real cell instead of no-op'ing.
    /// Write a field by id on a receiver that may be a heap `Value::Ref` OR an
    /// arena `Value::Obj` (the dual of `read_value_field`). Returns false if the
    /// receiver is neither. Used by the field-updater / atomic natives so a CAS on
    /// an arena-allocated object updates the real cell instead of no-op'ing.
    pub(crate) fn write_value_field(&mut self, recv: &Value, id: u32, value: Value) -> bool {
        match recv {
            Value::Ref(h) => { self.heap.put_field_hint(*h, id, value, 0); true }
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => { crate::opcodes::arena_putfield(*p, id, &value); true }
            _ => false,
        }
    }
}

/// Per-site refusal accounting for `jit_fu_invoke` (see its comment).
///
/// Direct-mapped and lossy on purpose: a collision costs a site its history, never correctness —
/// the worst outcome is that a hopeless site keeps trying or a good one stops, and both are
/// performance, not semantics. `RAVA_NO_FU_RETIRE` disables retirement entirely.
const FU_SITES: usize = 8192;
const FU_RETIRE_AT: i16 = -64;
static FU_TAG: [std::sync::atomic::AtomicU64; FU_SITES] =
    [const { std::sync::atomic::AtomicU64::new(0) }; FU_SITES];
static FU_SCORE: [std::sync::atomic::AtomicI32; FU_SITES] =
    [const { std::sync::atomic::AtomicI32::new(0) }; FU_SITES];

#[inline]
fn fu_site_slot(ptr: usize, cp: u16) -> usize {
    let k = (ptr as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (cp as u64).wrapping_mul(0x517C_C1B7);
    ((k >> 17) as usize) & (FU_SITES - 1)
}

#[inline]
fn fu_site_key(ptr: usize, cp: u16) -> u64 {
    (ptr as u64) ^ ((cp as u64) << 48)
}

/// `RAVA_NO_FAST_JITNAT=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) restores this round's
/// pre-existing sequence in `jit_natives.rs` byte for byte: the `IntArrayList` publisher in
/// [`Vm::fu_int_view`] that wrote `it/unimi/dsi/fastutil/ints/IntArrayList`'s `a`/`size` interner
/// ids into `crate::jit::DALIST_A_FID` / `DALIST_SIZE_FID` — cells whose contract, and whose only
/// three consumers (`emit::collections`'s inline `getDouble`, and the `getDouble`/`size` shapes in
/// `emit::iterator_inline`), are `it/unimi/dsi/fastutil/doubles/DoubleArrayList`.
///
/// WHAT IT WITHDRAWS, precisely: with the switch set, an `IntArrayList` view publishes again and,
/// because every writer of those cells is first-writer-wins, locks out the two publishers that
/// hold a real `DoubleArrayList` (`Vm::fu_double_view_shared` and `Vm::jit_run`'s compile-time
/// resolve). `dalist_ready()` then reports TRUE with ids belonging to another class, the three
/// arms are emitted, and each one runs its class-word guard, misses the layout slot for a field
/// the receiver does not have, fails its `is_arr` test and jumps to the callback it exists to
/// avoid. The guard can never answer wrongly — every value it reads is re-verified before use —
/// so the only symptom is a fast path that is emitted, entered and never taken.
///
/// Read once. A `var_os` per call is the defect this whole area is about; `emit_more_disabled` is
/// itself memoised, so this is one `OnceLock` for the pair.
///
/// TABLE SIZE: one `OnceLock<bool>`, 16 bytes of static, process-lifetime.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn fast_jitnat_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_JITNAT").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Is the field-id memo in its PRODUCTION configuration — on, and with the engagement probe off?
///
/// ⚠**THE TWIN OF `fu_view::probe_enabled`, AND IT IS SPELLED HERE BY NAME.** That function is
/// private to its module, so the variable is read again rather than borrowed. If the probe is
/// ever renamed, rename it here too: the failure mode is silent under-counting in the probe's
/// own report, never a wrong field id.
///
/// TABLE SIZE: one `OnceLock<bool>`, 16 bytes of static, process-lifetime.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn fu_memo_fast() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        super::jit_call::fast_jitcall_enabled()
            && fu_view::memo_enabled()
            && std::env::var_os("RAVA_FU_FIELD_IDS_PROBE").is_none()
    })
}

/// Is ANYTHING in `jit_fu_invoke`'s accounting block wired at all?
///
/// The four gates it ORs together — site retirement plus the three `RAVA_DIAG` counters — are
/// each a process-constant `OnceLock<bool>` over the environment, so their disjunction is as
/// constant as they are. One acquire load answers for all four, and a false answer proves the
/// whole block is dead for the run: no site hashing, no tag/score traffic, no counter reads.
///
/// TABLE SIZE: one `OnceLock<bool>`, 16 bytes of static, process-lifetime. It is a REPLACEMENT
/// for reads, not a keyed lookup, so there is no probe here to lose against a scan.
///
/// Registering the three counters inside the initialiser is deliberate: it happens once, off the
/// call path, and it makes them appear in a `RAVA_DIAG` dump even on a run where the fast arm
/// means their `inc()` is never reached.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn fu_call_diag() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // Kill switch: run the original unconditional sequence forever, so one binary A/Bs both.
        if !super::jit_call::fast_jitcall_enabled() {
            return true;
        }
        fu_retire_enabled()
            || crate::diag::fu_served().armed()
            || crate::diag::fu_missed().armed()
            || crate::diag::fu_site_skipped().armed()
            || super::fu_miss_census::enabled()
    })
}

/// ⛔**BUILT, MEASURED, AND OPT-IN: THE PENALTY AND THE PRIZE ARE THE SAME SIZE.**
///
/// The problem it solves is real and measured: `fu_routable` admits a site from its DECLARED
/// owner at compile time, so a site typed `List` meets guest lists, sets and views at runtime
/// that `fu_fastpath` must refuse — and a refusal pays the attempt AND the general path. Widening
/// the iterator admission by one owner made `bench/StreamApplyCost.java` 25% faster and the boot
/// **0.7 s slower** (median wall 37.50 against 36.80 for the narrow form, same OFF arm both
/// rounds). Retiring a site that never hits removes exactly that penalty.
///
/// It does. And then the widened admission measures NEUTRAL rather than positive:
///
/// ```text
/// widened admission, retirement ON    wall 38.5 38.2 41.9 37.8   median 38.35
/// narrow admission,  retirement ON    wall 39.8 38.8 38.8 38.2   median 38.80
/// retirement ON vs OFF (widened)      36.2 36.2 36.2  vs  35.7 36.3 36.7
/// ```
///
/// ⭐So the mechanism works and buys nothing: it converts a 0.7 s loss into a wash, because what
/// the widening gains on the sites that hit, it loses on the sites that do not — and retiring the
/// second group only stops the bleeding, it does not create a win.
///
/// ⛔**RE-OPENED ON A NEW POPULATION, AND THE SIGN DID NOT SURVIVE THE SECOND SERIES.**
/// The verdict above was taken against a WIDENED admission — the retired sites were ones an
/// experiment had just added. The arena refusal census (`FUO:` rows, added the same day this was
/// re-tried) names a very different population under the DEFAULT admission: 1,816,304 refused
/// calls on a datapack run, led by `Reference2ObjectArrayMap#put` at 709,719, each paying the
/// attempt AND the general dispatch — roughly 700 ns where a served call costs 110. The
/// arithmetic says ~1.1 s of a ~24 s run, so it looked like a real target.
///
/// ```text
/// RAVA_FU_RETIRE=1 vs default    med 24.64 vs 25.03  = -1.56%   min -1.73%   5 of 6 pairs
/// default(ON) vs RAVA_NO_FU_...  med 24.50 vs 24.28  = +0.89%   min -0.79%   sign flipped
/// ```
///
/// Same box, same workload, same binary, one run apart: **the median and the min disagree in
/// SIGN inside the second series.** So the mechanism stays opt-in — not because it is wrong, but
/// because 1.5% is inside this instrument's floor and a result that does not survive more pairs
/// is not a result. ⭐What DID come out of it is durable: "measured, buys nothing" is a statement
/// about a POPULATION, and the refusal census is what makes a population nameable.
#[inline]
fn fu_retire_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FU_RETIRE").is_some())
}

/// ⚠**THE TAG IS NOT OPTIONAL.** Without it two sites that collide share a score, and one
/// hopeless site could retire a hot one — a silent performance fault with no symptom to find it
/// by. The tag makes a collision cost the LOSER its history and nothing else.
#[inline]
fn fu_site_retired(slot: usize, key: u64) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    fu_retire_enabled()
        && FU_TAG[slot].load(Relaxed) == key
        && FU_SCORE[slot].load(Relaxed) <= FU_RETIRE_AT as i32
}

#[inline]
fn fu_site_note(slot: usize, key: u64, hit: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if !fu_retire_enabled() {
        return;
    }
    if FU_TAG[slot].load(Relaxed) != key {
        // Claim the slot for this site, with a clean history.
        FU_TAG[slot].store(key, Relaxed);
        FU_SCORE[slot].store(0, Relaxed);
    }
    if hit {
        // One success keeps a site forever: a mixed site must never retire.
        FU_SCORE[slot].store(i32::MAX / 2, Relaxed);
    } else {
        let s = FU_SCORE[slot].load(Relaxed);
        if s > FU_RETIRE_AT as i32 {
            FU_SCORE[slot].store(s - 1, Relaxed);
        }
    }
}
