//! Class identity for a value, and the memo that keeps instanceof from re-deriving it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// Clone a member ref's `(owner, name, descriptor)` out of the frame's pool so
/// later mutable frame access doesn't fight the borrow.
pub(crate) fn ref_parts(frame: &Frame, index: u16) -> Result<(Arc<str>, Arc<str>, Arc<str>), VmError> {
    ref_parts_of(&frame.class, index)
}

/// `ref_parts` without a `Frame`. The opcode bodies are being decoupled from `&mut Frame` so the
/// interpreter's hot loop can execute them WITHOUT returning to `run` (ledger row 349: `getfield`
/// alone ends **23.8% of 1.29 billion hot-run exits**), and `run_hot` holds the frame destructured
/// into disjoint borrows — it has `class`, but it cannot produce a `&Frame`.
pub(crate) fn ref_parts_of(
    class: &Arc<crate::class_loader::LoadedClass>,
    index: u16,
) -> Result<(Arc<str>, Arc<str>, Arc<str>), VmError> {
    let s = ref_site_ref(class, index)?;
    Ok((s.owner.clone(), s.name.clone(), s.descriptor.clone()))
}

/// The per-class site table, built on the first member-ref op that class interprets.
///
/// One slot per constant-pool index, EIGHT BYTES each — a raw pointer, not the `OnceLock<Arc<_>>`
/// the first dense attempt used. Classes that never reach the interpreter never allocate it at
/// all, so the table is bounded by executing classes rather than loaded ones.
#[inline(always)]
fn site_slots(cls: &crate::class_loader::LoadedClass) -> &[std::sync::atomic::AtomicPtr<crate::class_loader::SiteRef>] {
    cls.site_slots.get_or_init(|| {
        (0..cls.file.pool.len())
            .map(|_| std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()))
            .collect()
    })
}

/// The resolved site, BORROWED, with no lock and no hash — two loads and a null test.
///
/// GIL-free workers share this table. Publication is a `compare_exchange`, and the value is a pure
/// function of the IMMUTABLE constant pool, so racing writers build identical sites and it does not
/// matter which one wins; the loser's box is dropped on the spot. This is why GAP-S09's
/// `debug_assert_not_gilfree` is absent here and MUST NOT be restored — it guarded a frozen-table
/// mutation, and this is not one.
pub(crate) fn ref_site_ref<'a>(cls: &'a crate::class_loader::LoadedClass, index: u16) -> Result<&'a crate::class_loader::SiteRef, VmError> {
    if let Some(slot) = site_slots(cls).get(index as usize) {
        let p = slot.load(std::sync::atomic::Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: a published slot holds a leaked `Box<SiteRef>` owned by this class, freed
            // only in `Drop for LoadedClass`, and `cls` outlives the returned borrow.
            return Ok(unsafe { &*p });
        }
    }
    build_site(cls, index)
}

/// First execution of one call site, per class. Cold by construction: every later execution of the
/// same site takes the load above.
#[cold]
fn build_site<'a>(cls: &'a crate::class_loader::LoadedClass, index: u16) -> Result<&'a crate::class_loader::SiteRef, VmError> {
    let r = cls.file.pool.member_ref(index).ok_or(VmError::BadConstant(index))?;
    let site = Box::into_raw(Box::new(crate::class_loader::SiteRef {
        owner: Arc::from(r.owner),
        name: Arc::from(r.name),
        descriptor: Arc::from(r.descriptor),
        argc: crate::descriptor::arg_count(r.descriptor) as u32,
        slots: crate::descriptor::arg_slots(r.descriptor) as u32,
        resolved: std::sync::OnceLock::new(),
        plan: std::sync::OnceLock::new(),
    }));
    match site_slots(cls).get(index as usize) {
        Some(slot) => match slot.compare_exchange(
            std::ptr::null_mut(),
            site,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        ) {
            // SAFETY (both arms): the surviving pointer is a live `Box<SiteRef>` owned by the
            // class's slot table; the losing box is dropped here and never handed out.
            Ok(_) => Ok(unsafe { &*site }),
            Err(winner) => {
                drop(unsafe { Box::from_raw(site) });
                Ok(unsafe { &*winner })
            }
        },
        // A member-ref index past the end of the pool cannot be reached by valid bytecode; serve
        // it rather than panicking, and let the box leak on that impossible path.
        None => Ok(unsafe { &*site }),
    }
}

pub(crate) const CLASS_KEY_STATIC_TAG: u64 = 1 << 62;

thread_local! {
    /// `(caller class ptr, cp, receiver class identity)` → answer. Same key shape
    /// and the same stability argument as `tlcache::REF_INLINE`/`OBJ_INLINE`: the
    /// caller class is never unloaded, the class-name interner never evicts, and an
    /// arena class id is a registry index — none of the three is an object address
    /// or a handle, so `GC_EPOCH`/`HANDLE_EPOCH` do not apply and a swept-and-reused
    /// handle simply presents its NEW class identity.
    pub(crate) static IOF_MEMO: std::cell::RefCell<crate::FastMap<(usize, u16, u64), bool>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

#[inline]
pub(crate) fn iof_count(c: &std::sync::atomic::AtomicU64) {
    if crate::fallback_tally::enabled() {
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Cheap, stable class identity for the receiver shapes whose `instance_of`
/// answer is decided by the class-name WALK alone. `None` = not memoisable, and
/// every `None` shape is one of:
///   * an O(1) early return in `instance_of` already (`Null`, `Array`, `Str`, the
///     unboxed primitives) — nothing to memoise, and
///   * a LAMBDA, whose answer comes from the per-OBJECT functional-interface
///     record (`$fi` on a handle, the fi cell on an arena lambda) and therefore
///     is NOT a function of the class: handle lambdas all share the one interned
///     `$Lambda` class name, and an arena lambda's class id encodes its CAPTURE
///     KINDS, not its interface. A class-keyed entry would answer those wrongly.
pub(crate) fn instanceof_class_key(heap: &crate::heap::Heap, value: &Value) -> Option<u64> {
    match value {
        // `$fi` is written only on `alloc_lambda` handles, so `!is_lambda` proves
        // the receiver cannot carry one and its answer comes from the walk.
        Value::Ref(h) if !heap.is_lambda(*h) => {
            let p = heap.class_name_ptr(*h);
            (p != 0).then_some(p as u64 | IOF_REF_TAG)
        }
        // Arena receiver: the class id IS the identity, and it decides the arena
        // STRING arm too (its own registered class id ⇒ a constant answer).
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            let cid = crate::nextgen::arena_class_id(*p);
            (!crate::nextgen::is_lambda_cid(cid)).then_some(cid as u64)
        }
        _ => None,
    }
}

/// ⭐**ONE SWITCH FOR THE WHOLE CAST / TYPE-TEST BLOCK** (`RAVA_NO_FAST_CASTS=1`).
///
/// Covers the direct-mapped dispatch-class cache below, the widened cast key, the
/// `(sub, sup)` subtype memo and the `aastore` memo in `casts.rs`. Also honours the
/// block-level `RAVA_NO_EMIT_MORE`, so one variable turns the whole rewrite off.
pub(crate) fn fast_casts_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_CASTS").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Tag bits for the two receiver spaces the CAST key adds on top of
/// `instanceof_class_key`'s three (see `IOF_REF_TAG`). Bit 63 is a heap `Ref`
/// name pointer, bit 62 a `&'static str` address, bit 61 an array ELEMENT name
/// pointer and bit 60 an unboxed scalar; an arena class id is untagged and always
/// below 2^32. User-space pointers leave all four bits clear, so the five spaces
/// are disjoint by construction.
const CAST_ARRAY_TAG: u64 = 1 << 61;
const CAST_SCALAR_TAG: u64 = 1 << 60;

/// Receiver identity for the THROWING type tests (`checkcast`, `aastore`,
/// `Class.cast`) — strictly wider than [`instanceof_class_key`], which exists to
/// key the LENIENT `instanceof` and must not move.
///
/// COMPLEXITY: a `checkcast` of a `String`, of an unboxed primitive or of an
/// ARRAY had NO key at all, so every one of them re-ran `instance_of_tri` —
/// for an array target that is a full element walk, and for the closed-set arms
/// a `ClassLoader::get_or_load` of the target. That WAS O(hierarchy) per
/// EXECUTION; it IS one memo probe per (site, receiver identity) pair, and what
/// grows is the number of distinct pairs, not the number of executions.
///
/// Each added shape is keyed by exactly what `instance_of_tri` reads from it:
///   * `Value::Str` and the four unboxed numerics answer by SET MEMBERSHIP on the
///     target alone (`STRING_SUPERS` / `boxed_supers`), so the value contributes
///     nothing beyond its variant — one constant per variant is a complete key;
///   * `Value::Array` is read only through `Heap::array_element`, whose `&str`
///     comes from the array-element interner (`array_element_names`, push-only,
///     one `Arc<str>` per distinct descriptor). Its DATA pointer is therefore a
///     stable bijection with the element type for the life of the heap — the same
///     argument `IOF_MEMO` makes for the class-name interner.
/// Lambdas stay unkeyed for the reason `instanceof_class_key` states: their answer
/// comes from a per-OBJECT functional-interface record, not from their class.
pub(crate) fn cast_class_key(heap: &crate::heap::Heap, value: &Value) -> Option<u64> {
    match value {
        Value::Str(_) => Some(CAST_SCALAR_TAG),
        Value::Int(_) => Some(CAST_SCALAR_TAG | 1),
        Value::Long(_) => Some(CAST_SCALAR_TAG | 2),
        Value::Float(_) => Some(CAST_SCALAR_TAG | 3),
        Value::Double(_) => Some(CAST_SCALAR_TAG | 4),
        Value::Array(h) => heap.array_element(*h).map(|e| e.as_ptr() as u64 | CAST_ARRAY_TAG),
        _ => instanceof_class_key(heap, value),
    }
}

/// Slots in the direct-mapped dispatch-class cache. **128 slots x 24 B
/// (`u64` tag + `Arc<str>` fat pointer, the `Option` niche-folded into the Arc) =
/// 3 072 B** — a quarter of a 32 KB L1 at worst, and one line touched per probe.
/// Deliberately NOT bigger: this tree has a recorded regression from a hot table
/// that grew 2.5 KB -> 14 KB and measured +6.8% WORSE, and the receiver population
/// of a boot is ~1 590 classes with a heavily skewed working set.
const FAST_CLASS_SLOTS: usize = 128;

thread_local! {
    /// receiver class identity -> its dispatch class name, DIRECT-MAPPED.
    ///
    /// The twin of `VIRT_CLASS` (`names.rs`) and deliberately a different
    /// instrument. That memo removed all 24M per-call `Arc<str>` builds and
    /// measured NEUTRAL, because it charged a `RefCell` borrow plus a hash probe
    /// plus an atomic refcount for an allocation mimalloc serves from a
    /// thread-local heap: comparable costs, so 24M of them cancelled. This one
    /// keeps the removal and drops the hash — a multiply, a shift, one array
    /// index and one `u64` compare — leaving only the refcount bump. A colliding
    /// key simply evicts and rebuilds, i.e. degrades to today's behaviour.
    static FAST_CLASS: std::cell::RefCell<[Option<(u64, Arc<str>)>; FAST_CLASS_SLOTS]> =
        std::cell::RefCell::new(std::array::from_fn(|_| None));
}

/// Mix the tag bits down into the index: `Ref` keys differ only in the low bits of
/// a 16-byte-aligned pointer and arena ids are dense small integers, so the raw
/// low bits alone would cluster.
#[inline(always)]
fn fast_class_slot(key: u64) -> usize {
    (key.wrapping_mul(crate::fxhash::FX_SEED) >> 40) as usize & (FAST_CLASS_SLOTS - 1)
}

#[inline]
fn fast_class_get(key: u64) -> Option<Arc<str>> {
    FAST_CLASS.with(|t| {
        let t = t.borrow();
        match &t[fast_class_slot(key)] {
            Some((k, name)) if *k == key => Some(name.clone()),
            _ => None,
        }
    })
}

#[inline]
fn fast_class_put(key: u64, name: &Arc<str>) {
    FAST_CLASS.with(|t| {
        t.borrow_mut()[fast_class_slot(key)] = Some((key, name.clone()));
    });
}

/// Memoised dispatch class name; `build` runs only on a miss (and always, when
/// the flag is off — same value either way, so the gauntlet below is unchanged).
///
/// COMPLEXITY: `build` WAS run per CALL — an `Arc<str>` allocation and a copy of
/// a name that is a CONSTANT of the receiver's class (for an arena receiver, an
/// `Arc` clone out of the class registry on top). It IS one array probe per call
/// and one build per class. What grows is the number of distinct receiver
/// classes (~1 590 in a boot), not the number of virtual calls (~24 000 000).
pub(crate) fn dispatch_class(key: u64, build: impl FnOnce() -> Arc<str>) -> Arc<str> {
    virt_class_stats::note_call();
    if fast_casts_enabled() {
        if let Some(hit) = fast_class_get(key) {
            return hit;
        }
        virt_class_stats::note_build();
        let built = build();
        fast_class_put(key, &built);
        return built;
    }
    if !virt_class_memo_enabled() {
        virt_class_stats::note_build();
        return build();
    }
    if let Some(hit) = crate::opcodes::names::VIRT_CLASS.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    virt_class_stats::note_build();
    let built = build();
    crate::opcodes::names::VIRT_CLASS.with(|c| {
        c.borrow_mut().insert(key, built.clone());
    });
    built
}

/// `dispatch_class` for a source that may not resolve (the arena class registry).
/// A `None` is never cached — an unresolvable id is not a stable answer worth a slot.
pub(crate) fn dispatch_class_opt(key: u64, build: impl FnOnce() -> Option<Arc<str>>) -> Option<Arc<str>> {
    virt_class_stats::note_call();
    if fast_casts_enabled() {
        if let Some(hit) = fast_class_get(key) {
            return Some(hit);
        }
        virt_class_stats::note_build();
        let built = build()?;
        fast_class_put(key, &built);
        return Some(built);
    }
    if !virt_class_memo_enabled() {
        virt_class_stats::note_build();
        return build();
    }
    if let Some(hit) = crate::opcodes::names::VIRT_CLASS.with(|c| c.borrow().get(&key).cloned()) {
        return Some(hit);
    }
    virt_class_stats::note_build();
    let built = build()?;
    crate::opcodes::names::VIRT_CLASS.with(|c| {
        c.borrow_mut().insert(key, built.clone());
    });
    Some(built)
}

/// Concrete runtime class of an object value — a legacy heap handle (`Ref`) or
/// an arena pointer (`Obj`) under next-gen-gc. `None` for non-objects.
pub(crate) fn object_class_name(vm: &mut Vm, v: &Value) -> Option<String> {
    match v {
        Value::Ref(h) => vm.heap.class_of(*h).map(str::to_string),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).map(|s| s.to_string()),
        _ => None,
    }
}
