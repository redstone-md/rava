//! The structural trivial-getter fast path — the fourth rung after the ring arm.
//!
//! Serves a zero-argument `()I`/`()Z` instance call whose RESOLVED target's own bytecode is
//! the exact five-byte field read (`aload_0; getfield; ireturn`), answered at the
//! `jit_call_body` choke with no synthetic frame and no `opcodes::invoke*` gauntlet — and
//! only when the receiver carries no native backing (the structural gate in the arm; a
//! constant body is refused outright). This is the JIT_CALLBACK population's predicate family: a
//! compiled caller whose zero-arg int/boolean call falls through direct-call and PIC to the
//! choke would otherwise build a frame to run five bytes.

use crate::*;

// Served, and only served: the counter is inert unless a price diagnostic is armed (the
// `diag_counter!` construction returns a disarmed counter when nothing is instrumented, so
// `.inc()` folds to nothing in production).
crate::diag_counter!(
    getter_fp_served,
    "price",
    "getter_fp_served",
    "structural trivial-getter fastpath answered a call with no synthetic frame"
);

/// ⭐**KILL SWITCH FOR THE STRUCTURAL TRIVIAL-GETTER FAST PATH: `RAVA_NO_GETTER_FASTPATH=1`**
/// (or the block switch `RAVA_NO_EMIT_MORE=1`). Resolved once.
///
/// Distinct from the retired `RAVA_NO_TRIVIAL_GETTER` on purpose: that switch withdrew the
/// EMIT-side arm (recognising getters at compile time, whose first-writer receiver selection
/// violated no-adapt admission), while this is the CALL-side arm. Its admission is STRUCTURAL,
/// on the same argument as the ring arm: the site's own `(name, descriptor)` selector is
/// resolved against the receiver's actual class via the same `resolve_method` the general
/// path runs, and the resolved body must match the byte pattern exactly — through the same
/// retained recogniser the retired arm built (`jit::trivial_getter::trivial_getfield`,
/// read-only reuse; the retired record/emit tables stay untouched), and the receiver must
/// carry no native backing (the structural gate in the arm). No guest class, field, package
/// or method name is consulted anywhere. Off, every crossing takes the general path
/// byte-for-byte as before.
#[inline]
pub(crate) fn getter_fp_enabled() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var_os("RAVA_NO_GETTER_FASTPATH").is_none() && !crate::flags::emit_more_disabled()
    });
    *ON
}

/// Answer kinds for the memo rows. The kind is a property of the MATCHED BODY, never of the
/// site's descriptor: a `()Z` site whose body `ireturn`s an int field is answered with the
/// raw cell, exactly as the framed body computes it — `Z` is an int at the bytecode level
/// and this engine does no masking (the ring arm's `RING_INT` makes the same argument).
mod gk {
    /// The body is the five-byte field read; the answer is the receiver's int cell.
    pub(super) const FIELD_INT: u8 = 0;
    /// Verified structural refusal for exactly this (class, target, method) triple. Only
    /// permanent facts are memoised this way — bytecode, access flags, exception tables,
    /// constant-pool descriptors are class constants — so the verdict never goes stale. The
    /// native-backing gate is NOT among them: backing is minted lazily, so its absence is
    /// only true until it is minted, and the gate runs per-crossing before the memo read
    /// instead of being memoised (see the arm).
    pub(super) const REFUSAL: u8 = 2;
}

/// ⭐**THE TRIPLE-KEYED MEMO, SEPARATE FROM `ring_memo` ON PURPOSE.** Same shape, different
/// population: a (receiver class, resolved target, method) triple that memoises a ring
/// answer must never be read back as a getter answer. Direct-mapped and thread-local for
/// the ring table's own reasons: the crossing is per-thread, so the table needs no
/// synchronization, and a stale entry can only collide with another stable process-lifetime
/// identity, which re-derives and re-verifies against the immutable bytecode. 128 slots
/// (the ring table's 64, widened for this population); the triple key always ends `| 1`,
/// so `u64::MAX` can never alias a live row.
mod getter_memo {
    const SLOTS: usize = 128;
    const EMPTY: u64 = u64::MAX;

    struct Table {
        key: [u64; SLOTS],
        kind: [u8; SLOTS],
        /// `FIELD_INT`: the field id. `REFUSAL`: unused.
        payload: [u32; SLOTS],
    }

    thread_local! {
        static T: std::cell::RefCell<Table> = const {
            std::cell::RefCell::new(Table {
                key: [EMPTY; SLOTS],
                kind: [0; SLOTS],
                payload: [0; SLOTS],
            })
        };
    }

    #[inline]
    fn slot(key: u64) -> usize {
        (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 57) as usize & (SLOTS - 1)
    }

    #[inline]
    pub(super) fn get(key: u64) -> Option<(u8, u32)> {
        let i = slot(key);
        T.with(|t| {
            let t = t.borrow();
            (t.key[i] == key).then(|| (t.kind[i], t.payload[i]))
        })
    }

    #[inline]
    pub(super) fn put(key: u64, kind: u8, payload: u32) {
        let i = slot(key);
        T.with(|t| {
            let mut t = t.borrow_mut();
            t.key[i] = key;
            t.kind[i] = kind;
            t.payload[i] = payload;
        });
    }
}

/// The triple key for `getter_memo`: the receiver-class key mixed with the resolved target's
/// `Arc` pointer and the method's own index in the target's method table. All three halves
/// are stable process-lifetime identities — the same mixing the ring table uses, in a table
/// of its own. The index half is what keeps sibling trivial getters on one class apart
/// (getX/getY/getZ share the declaring `Arc` and the five-byte shape; only the index
/// differs), so their verdicts can no longer alias.
#[inline]
fn getter_memo_key(recv_key: u64, target_ptr: u64, midx: u32) -> u64 {
    (recv_key ^ target_ptr.rotate_left(17) ^ (midx as u64).rotate_left(43))
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        | 1
}

/// The receiver's class name without an allocation. Heap receivers borrow the heap's own
/// `&str` directly; arena receivers hold the loader's `Arc<str>` — a refcount bump, no
/// copy. (The ring arm pays a `to_string()` per crossing for the same name.) Every use is a
/// `&self` call, so the immutable borrow coexists with the rest of the arm.
enum ClsName<'a> {
    Heap(&'a str),
    Arena(Arc<str>),
}

impl ClsName<'_> {
    #[inline]
    fn as_str(&self) -> &str {
        match self {
            ClsName::Heap(s) => s,
            ClsName::Arena(s) => &**s,
        }
    }
}

/// A `synchronized` body's monitor is invocation semantics this arm does not reproduce; a
/// static body reached by a virtual site must raise ICCE on the general path. Both flags are
/// immutable class constants, so refusals on them are memoised.
const ACC_STATIC: u16 = 0x0008;
const ACC_SYNCHRONIZED: u16 = 0x0020;

impl Vm {
    /// ⭐**THE STRUCTURAL TRIVIAL-GETTER ARM.** See `getter_fp_enabled` for the admission
    /// argument; the mechanics:
    ///
    /// * The triple key is (receiver class identity, resolved target `Arc` pointer, the
    ///   method's own index in the target's method table) — all stable process-lifetime
    ///   identities, the same halves the whole engine keys caches on. The index half pins
    ///   exactly the method the body verification ran on, so one getter can serve while a
    ///   sibling target on the same class re-derives and is refused by the byte compare.
    /// * A REFUSAL is memoised only for facts that are pure functions of the triple —
    ///   access flags, a non-empty exception table, a shape mismatch, a non-`ireturn`
    ///   tail, a Fieldref that is not an int-family field of the declaring class. Three
    ///   declines stay UNMEMOISED because their verdicts are NOT functions of the key:
    ///   the native-backing gate (backing is minted lazily, so today's absence may be
    ///   tomorrow's presence — the gate re-runs every crossing BEFORE the memo read, so a
    ///   row derived under absent backing is retired the moment the backing appears), the
    ///   arena slot check (`nextgen::slot_of` caches only positive answers and layouts
    ///   are only ever appended to, so today's `None` may be tomorrow's `Some` — the same
    ///   reason the ring arm leaves its layout miss re-asked), and a null/absent
    ///   receiver-word (a defensive miss; the ring arm's convention).
    /// * `getter_read_int` carries the COW shadow parity of the general `getfield_value`,
    ///   so a GIL-free worker's shadowed write is visible to a served read (see its note).
    /// Refusal sense: `None` on any anomaly — kill switch, null receiver (its NPE is the
    /// general path's, raised with the frame this arm would not contribute to the trace), a
    /// worker's region abort, a native-backed receiver, a non-matching body, a non-`Int`
    /// cell (never coerced) — and the crossing continues to the frame exactly as before,
    /// so behaviour is unchanged in both directions by construction. This arm never throws.
    pub(crate) fn getter_fastpath(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        // The site's ACTUAL selector, from the caller's own pool. `raw.len() == 1` is the
        // hook's precondition (a zero-arg instance call), so `raw[0]` is the receiver.
        let m = caller.file.pool.member_ref(cp)?;
        // The whole site-level admission, from the descriptor alone: zero parameters and an
        // int-or-boolean return. The name is never consulted — the resolution below is the
        // gate, not a literal. Anything wider is declined before any resolve.
        if m.descriptor != "()I" && m.descriptor != "()Z" {
            return None;
        }
        // Decode the receiver. Null takes the general path.
        let recv = jit::unbox_ref(self, *raw.first()?, m.owner);
        // Receiver identity — the same two halves `ring_size_fastpath` encodes. The class
        // NAME rides alongside as a borrow (see `ClsName`).
        let (key, arena, cls) = match &recv {
            Value::Ref(h) => {
                let w = self.heap.object_class_word(*h)? as u64;
                if w == u64::MAX {
                    return None;
                }
                (w, false, ClsName::Heap(self.heap.class_of(*h)?))
            }
            Value::Obj(p) => {
                let cid = crate::nextgen::arena_class_id(*p);
                (
                    ((cid as u64) << 1) | 1,
                    true,
                    ClsName::Arena(crate::nextgen::class_name(cid)?),
                )
            }
            _ => return None,
        };
        // Resolve the site's selector against the receiver's ACTUAL class — the identical
        // dispatch the general path would perform. `Err` (a GIL-free worker's region abort)
        // and `Ok(None)` (no body anywhere in the chain) both decline to the general path.
        let Ok(Some(target)) = self.resolve_method(cls.as_str(), m.name, m.descriptor) else {
            return None;
        };
        // ⭐**THE STRUCTURAL NATIVE-BACKING GATE — the general path's routing, asked as a
        // field fact instead of a name.** A declined crossing continues to
        // `opcodes::invokevirtual` → `Vm::invoke` → `invoke_framed`, and for a receiver
        // OUR native layer owns (a `$map`/`$elems`-backed map, set or stream; an arena
        // map; a BiMap inverse view, which redirects through `$inverseof` to the forward
        // map) the general path answers from the native layer, not the body's declared
        // cell — a five-byte body this arm could otherwise serve from the raw cell would
        // produce a DIFFERENT (silent, wrong) answer. The presence of the backing IS the
        // routing fact, by the general path's own construction: the native map/set layer
        // reads `$map`/`$elems` on every call (and `map_id` lazily MINTS `$map` on first
        // touch, `fastutil::store` writes `$elems` at every mutation), so "backed" is
        // exactly the population the natives serve, and the probe is a field read by
        // interned id — no guest class, field or method name is consulted.
        //
        // THE PLACEMENT IS LOAD-BEARING: the gate runs EVERY crossing, AFTER the resolve
        // (so backing minted by re-entrant guest code in the resolve's class-load window
        // is visible) and BEFORE the memo read (so a `FIELD_INT` row derived under absent
        // backing is retired the moment the backing is minted — the memo can never answer
        // a stale serve). A decline here is UNMEMOISED precisely because backing is born
        // lazily: today's absence is not tomorrow's.
        //
        // An ABSENCE is a serve, not a hole: for the force-native families (fastutil
        // object maps, guava maps, the int sets) every method name — `<init>` included —
        // routes native, so no guest bytecode ever writes the declared cells (a fact that
        // holds exactly while `force_native_collection` keeps routing every name for
        // them) and the prepopulated default (0) is exactly the native layer's answer
        // for an empty backing. The two sides agree in both directions; the subclass
        // case is subsumed (an inherited method resolves into the native-routed base,
        // and the construction's `<init>`/first mutation mints the backing on the
        // subclass receiver itself, where this probe reads it). The one residual
        // native-vs-cell family is DoubleArrayList's `size()` intercept — every writer
        // of the `size` cell (the wrap natives and the class's own growth bytecode)
        // maintains `size <= a.length`, so the raw cell IS `min(size, phys)` here.
        match &recv {
            Value::Ref(h) => {
                if matches!(self.heap.get_field_syn(*h, crate::heap::Syn::Map), Value::Int(_))
                    || matches!(
                        self.heap.get_field_syn(*h, crate::heap::Syn::Elems),
                        Value::Array(_)
                    )
                    || matches!(
                        self.heap.field_id_opt("$inverseof"),
                        Some(id) if matches!(self.heap.get_field_by_id(*h, id), Value::Ref(_))
                    )
                {
                    return None;
                }
            }
            Value::Obj(p) => {
                // An arena map is OUR flat table (class-id identity, `rava/ArenaMap` —
                // a registry class, not a guest name); every other arena receiver is
                // probed by backing-id layout presence, the same structural fact the
                // serve's own Fieldref is checked against below.
                if crate::arena_map::is_arena_map(*p) {
                    return None;
                }
                let cid = crate::nextgen::arena_class_id(*p);
                let backed = [crate::heap::Syn::Map, crate::heap::Syn::Elems]
                    .into_iter()
                    .filter_map(|s| self.heap.syn_field_id(s))
                    .any(|id| crate::nextgen::slot_of(cid, id).is_some());
                if backed {
                    return None;
                }
            }
            _ => return None,
        }
        // THE TRIPLE KEY: (receiver class, resolved target, METHOD). The method's own index
        // in the target's method table is the half that keeps sibling trivial getters apart
        // (getX/getY/getZ on one class share the declaring `Arc` and the five-byte shape;
        // only the index differs), so their verdicts no longer alias. `None` is defensive —
        // `resolve_method` guarantees a body — and declines un-memoised, the ring arm's
        // convention for defensive misses. O(1) under the default `F_FAST2` gate.
        let midx = crate::tlcache::method_index(&target, m.name, m.descriptor)?;
        let mkey = getter_memo_key(key, Arc::as_ptr(&target) as u64, midx);
        if let Some((kind, payload)) = getter_memo::get(mkey) {
            // A memoised row is either a verified FIELD_INT serve or a permanent
            // structural refusal — both pure functions of the triple (see `gk`). The
            // backing gate above has already retired the native-layer families, which
            // is what keeps this read sound for backing born after the row was written.
            let payload = match kind {
                gk::FIELD_INT => payload,
                _ => return None,
            };
            let answer = self.getter_read_int(&recv, payload)?;
            getter_fp_served().inc();
            return Some(jit::pack_local(&Value::Int(answer)));
        }
        // — memo miss: verify the resolved body once —
        // `method_index` is guaranteed in range by its own defensive check, and
        // `resolve_method` guarantees a body; the `?` stays defensive either way.
        let mi = target.file.methods.get(midx as usize)?;
        let code = mi.code.as_ref()?;
        // Flag refusals are immutable facts — memoise (see `ACC_STATIC`/`ACC_SYNCHRONIZED`).
        if mi.access_flags & (ACC_STATIC | ACC_SYNCHRONIZED) != 0 {
            getter_memo::put(mkey, gk::REFUSAL, 0);
            return None;
        }
        // No exception handler may sit on the body we emulate. Immutable — memoise.
        if !code.exceptions.is_empty() {
            getter_memo::put(mkey, gk::REFUSAL, 0);
            return None;
        }
        let bc = &code.bytecode[..];
        // THE ONE SHAPE: `aload_0; getfield; <return>`. A constant body (`iconst;ireturn` —
        // the `trivial_const` family, ~259k DFU crossings a boot) is refused OUTRIGHT: the
        // con-vs-body overrides the general path carries (slf4j `is*Enabled` from the name,
        // netty `inEventLoop` from the name) all live on that shape, and the five-byte
        // field read is the shape whose answer equals the native layer's own cell read in
        // every family the backing gate passes. Refusing the constant body is the simple
        // rule that makes the arm's answer equal the body's by construction.
        let Some(fidx) = crate::jit::trivial_getter::trivial_getfield(bc) else {
            getter_memo::put(mkey, gk::REFUSAL, 0);
            return None;
        };
        // The recogniser admits `lreturn`/`freturn` and friends; a `()I`/`()Z` body can
        // only legally carry `ireturn`, so the tail check keeps a malformed wide-tailed
        // body on the general path. Immutable — memoise.
        if bc[4] != 0xac {
            getter_memo::put(mkey, gk::REFUSAL, 0);
            return None;
        }
        // The field the body reads, from the body's OWN Fieldref — not a literal. It must be
        // an int-family field (`I`/`Z`/`B`/`C`/`S` — all hold `Value::Int` cells in this
        // engine; there is no Bool/Byte variant and defaults are `Int(0)`) of the declaring
        // class itself, the ring arm's owner test: the serve returns the cell raw,
        // bit-identical to the framed body. Both are immutable — memoise.
        let fr = target.file.pool.member_ref(fidx)?;
        if !matches!(fr.descriptor, "I" | "Z" | "B" | "C" | "S")
            || fr.owner != target.file.this_class
        {
            getter_memo::put(mkey, gk::REFUSAL, 0);
            return None;
        }
        // The id comes from the body's own Fieldref, resolved exactly as the framed `getfield`
        // would resolve it.
        let fid = self.resolve_field_id(fr.owner, fr.name);
        // Arena layout check: an off-layout arena read is a PANIC, so a missing slot declines
        // UNMEMOISED and is re-asked next call — a layout is only ever appended to.
        if arena {
            let cid = (key >> 1) as u32;
            if crate::nextgen::slot_of(cid, fid).is_none() {
                return None;
            }
        }
        getter_memo::put(mkey, gk::FIELD_INT, fid);
        let v = self.getter_read_int(&recv, fid)?;
        getter_fp_served().inc();
        Some(jit::pack_local(&Value::Int(v)))
    }

    /// Read the receiver's int cell with the COW parity of the general path.
    ///
    /// ⭐**THE SHADOW COMES FIRST, exactly as `getfield_value` (opcodes/fields.rs) orders it:
    /// a GIL-free worker's shadowed `putfield` must be visible to this read, or the arm
    /// would answer a STALE shared cell the framed path would not. The conjunction is the
    /// general path's verbatim — `gilfree() && cow_gate()`, minus the owned-bypass/undo
    /// receiver — and on the serial boot `gilfree()` is a false TLS byte, so the consult
    /// costs that one load and short-circuits. Every served answer is a `FIELD_INT` cell
    /// read, so every served answer pays this consult.
    ///
    /// A non-`Int` value declines rather than coercing, from either source: the general path
    /// re-reads the same cell and produces exactly the answer it always produced (including
    /// any exception), with the frame this arm would not have contributed to the trace.
    fn getter_read_int(&self, recv: &Value, fid: u32) -> Option<i32> {
        if crate::parallel::gilfree()
            && crate::parallel::cow_gate()
            && !((crate::parallel::cow_owned_bypass() || crate::parallel::cow_owned_undo())
                && crate::parallel::cow_owned(recv))
        {
            if let Some(k) = crate::parallel::cow_obj_key(recv) {
                if let Some(v) = crate::parallel::shadow_get_field(k, fid) {
                    return match v {
                        Value::Int(n) => Some(n),
                        _ => None,
                    };
                }
            }
        }
        let v = match recv {
            Value::Ref(h) => self.heap.get_field_by_id(*h, fid),
            // The arena layout check happened at derive; the non-panicking variant still
            // declines (never coerces) if the slot is somehow absent.
            Value::Obj(p) => crate::opcodes::arena_getfield_opt(*p, fid)?,
            _ => return None,
        };
        match v {
            Value::Int(n) => Some(n),
            _ => None,
        }
    }
}
