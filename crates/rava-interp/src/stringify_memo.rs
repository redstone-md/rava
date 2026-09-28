//! Bounded, call-scoped memoization for structurally proven-pure `toString` trees.
//!
//! Recursive value graphs frequently stringify the same node more than once in
//! one outer `String.valueOf` call. Re-entering guest bytecode rebuilds the full
//! subtree and all intermediate strings each time. This cache exists only while
//! that outer call is active and stores results only after the VM's transitive
//! effect proof accepted the concrete `toString` implementation as pure.

use crate::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

const MAX_ENTRIES: usize = 8_192;
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// A cached render is identified by the object AND by whether it was produced
/// under a key-derivation scope: those renders carry interned tokens in place of
/// their nested subtrees, so they must never be served to an ordinary
/// `String.valueOf`.
#[derive(Clone, Copy, Hash, Eq, PartialEq)]
enum MemoKey {
    Heap(u32, bool),
    #[cfg(feature = "next-gen-gc")]
    Arena(u64, bool),
}


/// CROSS-CALL CACHE CENSUS (`RAVA_PERSIST_STAT`, default OFF, inert).
///
/// Every outcome is counted, including the ones that make the cache pure overhead. Two lessons
/// from this hunt are built in:
/// * a zero from a probe says the PROBE's path was not taken before it says anything about the
///   world - so "not admitted" is counted separately from "missed";
/// * a hit rate whose denominator excludes the cases you are worried about says nothing about
///   them - the earlier version of the map-key counter reported 99.7% while silently omitting
///   every rejected object.
///
/// 0 = call-scoped hit, 1 = cross-call hit, 2 = miss, 3 = stored, 4 = not admitted (mutable class
/// or unproven purity).
pub(crate) fn persist_stat(kind: usize) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_PERSIST_STAT").is_some()) {
        return;
    }
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static C: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
    C[kind].fetch_add(1, Relaxed);
    let v: Vec<u64> = C.iter().map(|a| a.load(Relaxed)).collect();
    let tot: u64 = v.iter().sum();
    if tot % 500_000 == 0 {
        eprintln!(
            "[PERSIST] call-hit={} cross-hit={} miss={} stored={} not-admitted={} | cross-hit of ALL {:.1}%",
            v[0], v[1], v[2], v[3], v[4],
            v[1] as f64 * 100.0 / tot as f64
        );
    }
}

/// Reflective-write epoch. `Field.set` on an INSTANCE bypasses the verifier and can therefore
/// write a `final` field, which is the one way a "provably immutable" object can change. It bumps
/// this; the cross-call cache folds it into its tag, so one such write drops every entry.
///
/// Deliberately NOT bumped by `VarHandle`/`AtomicReferenceFieldUpdater` writes: those are declared
/// against non-final fields by contract, so they cannot reach an object admitted by the all-final
/// rule. If that ever stops being true the bump belongs there too.
static IMMUT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn bump_immut_epoch() {
    IMMUT_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Tag for the cross-call cache. A collection can hand a live handle's index to a DIFFERENT
/// object, and a reflective write can set a final field, so both invalidate every entry.
#[cfg(feature = "next-gen-gc")]
fn persist_tag() -> u64 {
    crate::nextgen::gc_epoch()
        ^ IMMUT_EPOCH.load(std::sync::atomic::Ordering::Relaxed).rotate_left(32)
}
#[cfg(not(feature = "next-gen-gc"))]
fn persist_tag() -> u64 {
    IMMUT_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    /// (tag, key -> render) for provably-immutable objects; survives the outer call.
    static PERSIST: RefCell<(u64, HashMap<MemoKey, Box<str>>)> =
        RefCell::new((u64::MAX, HashMap::new()));
}

#[derive(Default)]
struct MemoCache {
    values: HashMap<MemoKey, String>,
    bytes: usize,
}

impl MemoCache {
    fn clear(&mut self) {
        self.values.clear();
        self.bytes = 0;
    }

    fn get(&self, key: MemoKey) -> Option<String> {
        self.values.get(&key).cloned()
    }

    fn insert(&mut self, key: MemoKey, rendered: &str) {
        if self.values.contains_key(&key)
            || self.values.len() >= MAX_ENTRIES
            || rendered.len() > MAX_BYTES.saturating_sub(self.bytes)
        {
            return;
        }
        self.bytes += rendered.len();
        self.values.insert(key, rendered.to_owned());
    }
}

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    static CACHE: RefCell<MemoCache> = RefCell::new(MemoCache::default());
    /// Monotonic count of renders that could NOT be proven pure. A caller brackets
    /// its own render with two reads: an unchanged mark proves that every nested
    /// `toString` inside it was itself proven pure, which is the transitive side
    /// condition a synthesized record accessor needs (it stringifies its reference
    /// components). Dynamic over the ACTUAL component values, so it never has to
    /// close over a declared type's whole override set.
    static IMPURE: Cell<u64> = const { Cell::new(0) };
}

/// Read the impurity mark (see `IMPURE`).
pub(crate) fn impure_mark() -> u64 {
    IMPURE.with(Cell::get)
}

/// Record that the render just finished could not be proven pure.
pub(crate) fn note_impure() {
    IMPURE.with(|c| c.set(c.get().wrapping_add(1)));
}

/// RAII scope for one `value_to_string` frame. Nested guest `toString` calls
/// share the outer cache; the last returning frame clears it unconditionally.
pub(crate) struct StringifyMemo {
    active: bool,
    key: Option<MemoKey>,
}

impl StringifyMemo {
    pub(crate) fn enter(value: &Value) -> Self {
        let active = enabled() && crate::frames::native_depth() > 0;
        crate::residue_probe::memo_event(if active { "active" } else { "inactive(no-native-depth)" });
        Self::enter_with_active(value, active)
    }

    fn enter_with_active(value: &Value, active: bool) -> Self {
        if !active {
            return Self { active, key: None };
        }

        DEPTH.with(|depth| {
            if depth.get() == 0 {
                CACHE.with(|cache| cache.borrow_mut().clear());
            }
            depth.set(depth.get() + 1);
        });

        Self {
            active,
            key: memo_key(value),
        }
    }

    pub(crate) fn lookup(&self) -> Option<String> {
        let key = self.key?;
        let hit = CACHE.with(|cache| cache.borrow().get(key));
        if hit.is_some() {
            crate::residue_probe::memo_event("lookup hit");
            persist_stat(0);
            return hit;
        }
        // Cross-call cache for objects the caller proved immutable. Tagged, so a collection or a
        // reflective write drops every entry at once rather than leaving a stale render behind.
        let tag = persist_tag();
        let p = PERSIST.with(|c| {
            let m = c.borrow();
            if m.0 == tag { m.1.get(&key).map(|s| s.to_string()) } else { None }
        });
        crate::residue_probe::memo_event(if p.is_some() { "lookup hit (persist)" } else { "lookup miss" });
        persist_stat(if p.is_some() { 1 } else { 2 });
        p
    }

    /// Whether a subsequent `store_if_proven` could store anything. When false,
    /// the caller can skip computing the (expensive) purity proof entirely — its
    /// result would be discarded. Behaviour-identical dead-work elimination.
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    /// Store into the CALL-SCOPED cache, and additionally into the cross-call one when the caller
    /// has proven the object immutable.
    ///
    /// The call-scoped cache clears every time the outer `String.valueOf` unwinds, which is right
    /// for a mutable object and wasteful for one that cannot change: the boot re-renders the same
    /// `Direction` (six instances exist) 219,142 times, and the render is 952 ns. `immutable` means
    /// the caller checked that EVERY instance field of the concrete class carries ACC_FINAL, so
    /// the verifier forbids any `putfield` to it outside the declaring class's `<init>` and the
    /// render is fixed for the object's lifetime.
    ///
    /// `proven_pure` is still required and still does the heavier job: it says the `toString`
    /// itself has no side effects worth repeating. Immutability alone would not justify reusing a
    /// render whose production mattered.
    pub(crate) fn store_if_proven_ext(&self, proven_pure: bool, immutable: bool, rendered: &str) {
        self.store_if_proven(proven_pure, rendered);
        if !self.active || !proven_pure || !immutable {
            persist_stat(4);
            return;
        }
        persist_stat(3);
        let Some(key) = self.key else { return };
        let tag = persist_tag();
        PERSIST.with(|c| {
            let mut m = c.borrow_mut();
            if m.0 != tag {
                m.0 = tag;
                m.1.clear();
            }
            if m.1.len() >= MAX_ENTRIES {
                m.1.clear();
            }
            m.1.insert(key, rendered.into());
        });
    }

    pub(crate) fn store_if_proven(&self, proven_pure: bool, rendered: &str) {
        crate::residue_probe::memo_event(match (self.active, proven_pure) {
            (true, true) => "store accepted",
            (true, false) => "store REJECTED (toString not proven pure)",
            _ => "store skipped (memo inactive)",
        });
        if !self.active || !proven_pure {
            return;
        }
        if let Some(key) = self.key {
            CACHE.with(|cache| cache.borrow_mut().insert(key, rendered));
        }
    }
}

impl Drop for StringifyMemo {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        DEPTH.with(|depth| {
            let next = depth.get().saturating_sub(1);
            depth.set(next);
            if next == 0 {
                CACHE.with(|cache| cache.borrow_mut().clear());
            }
        });
    }
}

fn memo_key(value: &Value) -> Option<MemoKey> {
    let keying = crate::key_intern::in_key_scope();
    match value {
        Value::Ref(handle) => Some(MemoKey::Heap(*handle, keying)),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(pointer) => Some(MemoKey::Arena(pointer.addr(), keying)),
        _ => None,
    }
}

fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("RAVA_STRINGIFY_MEMO")
            .map(|value| value != "0")
            .unwrap_or(true)
    })
}

#[cfg(test)]
mod tests {
    use super::{bump_immut_epoch, MemoCache, MemoKey, StringifyMemo, MAX_BYTES, MAX_ENTRIES};
    use crate::Value;

    #[test]
    fn bounded_cache_keeps_first_exact_result() {
        let mut cache = MemoCache::default();
        cache.insert(MemoKey::Heap(7, false), "first");
        cache.insert(MemoKey::Heap(7, false), "second");
        assert_eq!(cache.get(MemoKey::Heap(7, false)).as_deref(), Some("first"));
        assert_eq!(cache.bytes, 5);
    }

    #[test]
    fn bounded_cache_fails_open_at_limits() {
        let mut cache = MemoCache::default();
        cache.bytes = MAX_BYTES;
        cache.insert(MemoKey::Heap(1, false), "x");
        assert!(cache.values.is_empty());

        cache.bytes = 0;
        for handle in 0..MAX_ENTRIES as u32 {
            cache.values.insert(MemoKey::Heap(handle, false), String::new());
        }
        cache.insert(MemoKey::Heap(MAX_ENTRIES as u32, false), "x");
        assert_eq!(cache.values.len(), MAX_ENTRIES);
    }

    #[test]
    fn nested_scopes_share_then_outer_scope_clears() {
        let outer = StringifyMemo::enter_with_active(&Value::Ref(7), true);
        outer.store_if_proven(true, "cached");
        {
            let nested = StringifyMemo::enter_with_active(&Value::Ref(7), true);
            assert_eq!(nested.lookup().as_deref(), Some("cached"));
        }
        drop(outer);

        let next_outer = StringifyMemo::enter_with_active(&Value::Ref(7), true);
        assert!(next_outer.lookup().is_none());
    }

    /// THE INVALIDATION CONTRACT of the cross-call cache, tested directly.
    ///
    /// The call-scoped cache is safe because it dies with the call. The cross-call one is not: it
    /// deliberately outlives the call on the strength of "every instance field is final, so the
    /// verifier forbids a write". Reflection breaks that rule, which is why `Field.set` on an
    /// instance bumps an epoch folded into the cache tag. This asserts the consequence rather than
    /// the mechanism: a bumped epoch must make a previously-served render unreachable.
    ///
    /// A bit-exact boot gate cannot cover this - it never performs a reflective write to a final
    /// field - so a green gate would say nothing about the one hazard this cache introduces.
    #[test]
    fn reflective_write_invalidates_the_cross_call_render() {
        let v = Value::Ref(4242);
        {
            let memo = StringifyMemo::enter_with_active(&v, true);
            memo.store_if_proven_ext(true, true, "Direction.NORTH");
        }
        // A NEW outer call: the call-scoped cache is gone, the cross-call one should answer.
        {
            let memo = StringifyMemo::enter_with_active(&v, true);
            assert_eq!(
                memo.lookup().as_deref(),
                Some("Direction.NORTH"),
                "an immutable render must survive the call that produced it"
            );
        }
        // Reflection writes a final field somewhere in the program.
        bump_immut_epoch();
        {
            let memo = StringifyMemo::enter_with_active(&v, true);
            assert_eq!(
                memo.lookup(),
                None,
                "a reflective write must drop every cross-call render, not just the object's own"
            );
        }
    }

    /// A MUTABLE class must never reach the cross-call cache at all, even when its `toString` is
    /// proven pure - purity of the method is not immutability of the object.
    #[test]
    fn mutable_class_is_not_admitted_across_calls() {
        let v = Value::Ref(4243);
        {
            let memo = StringifyMemo::enter_with_active(&v, true);
            memo.store_if_proven_ext(true, false, "mutable");
        }
        let memo = StringifyMemo::enter_with_active(&v, true);
        assert_eq!(memo.lookup(), None, "immutable=false must not persist");
    }
}
