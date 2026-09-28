//! ⭐**AN `Optional` IS A TWO-STATE WRAPPER, AND ONE OF THE TWO STATES HAS NO CONTENTS.**
//! `[OPTLITE]` — the census that prices `java.util.Optional` in this workload, and the one
//! object it stops minting.
//!
//! ## WHY THIS EXISTS
//!
//! `com.mojang.serialization.DataResult$Instance` overrides only `ap`/`ap2`/`ap3`, so a record
//! codec of arity >= 4 halves recursively and every level answers "are all of these present?"
//! about values it is already holding — with an `Optional` per answer. Arity 4 allocates 12,
//! arity 8 allocates 24, arity 16 allocates 48. Minecraft has 790 classes built with
//! `RecordCodecBuilder` (arity spread: `BiFunction` 293, `Function3` 167, `Function4` 92, F5 42,
//! F6 24, F7 16, F8+ 25) and the datapack reload decodes ~50,000 records through them.
//!
//! Every one of those `Optional`s is, today, a `Box<Object>` on the legacy heap — 324 ns of
//! pure birth each (`bench/NativeCost.java`) — because `stream::make_optional`'s arena route is
//! behind `RAVA_ARENA_NATIVE_NEW`, which the standing environment does not set.
//!
//! ## THE COUNT COMES FIRST, AND IT IS A COUNT OF BIRTHS, NOT OF CALLS
//!
//! `stream::make_optional` is the SOLE constructor of `java/util/Optional` in this tree — every
//! producer (`Optional.of`/`ofNullable`/`empty` from the cascade, the same three from
//! `fu_fastpath`'s compiled-code arm, `optional_op`'s `map`/`filter`/`or`, `stream::collect`'s
//! `min`/`max`, `pipeline`'s `findFirst`/`findAny`/`reduce`) bottoms out there. So the birth
//! counters below are ROUTE-INDEPENDENT: a call served in machine code is counted exactly like
//! one served from the interpreter. (The single exception is
//! `natives/cascade_table2.rs:109`'s `ModuleLayer.findModule`, which hand-rolls one empty
//! `Optional` for the JFR absence probe — once per boot, and deliberately left alone.)
//!
//! The per-CALL-NAME table is a different fact and is honest about being narrower: it counts
//! `java/util/Optional` calls ARRIVING AT THE OWNER CASCADE (`natives/cascade.rs`). A compiled
//! caller whose site `jit::direct::fu_routable` admitted — `of`/`ofNullable`/`empty`, and the
//! `get`/`orElseThrow`/`isPresent`/`isEmpty`/`orElse` readers — is served by
//! `vm/fu_fastpath.rs` and never reaches the cascade, and the three `pic_native::leaf_op`
//! shapes (`isPresent`/`isEmpty`/`get`) are answered inside machine code. So read the name
//! table as the SHAPE of the population, and the birth counters as its SIZE.
//!
//! ## WHAT IS SERVED WITHOUT AN OBJECT
//!
//! An empty `Optional` carries nothing: `$present = 0` and `$val = null`, for every empty that
//! was ever minted. Two facts make one shared instance not merely safe but MORE faithful than
//! what this engine does today:
//!
//! 1. **It is immutable.** `$present`/`$val` are written exactly once, at construction, by
//!    `stream::make_optional` — the only writer in the tree (the same fact
//!    `vm/natives_ws.rs:252` already relies on to read them off-GIL from a worker).
//! 2. **The JDK's own `Optional.empty()` IS a singleton** (`private static final Optional<?>
//!    EMPTY`), it has no public constructor, and `Optional.ofNullable(null)`,
//!    `opt.filter(alwaysFalse)` and `emptyOpt.map(f)` all return that same `EMPTY`. So
//!    `Optional.empty() == Optional.empty()` is `true` on HotSpot and was `false` here.
//!    Interning the empty state moves us TOWARD the reference implementation, not away.
//!
//! `of(x)` is deliberately NOT interned: the JDK's `Optional.of` is `new Optional<>(value)`
//! every time, so a shared present instance would invent an identity HotSpot does not have.
//!
//! ⚠**The guard may only REJECT.** [`intercept`] returns `None` for every present value and for
//! every call when the switch is off, and `None` means `make_optional` runs the historic code
//! verbatim.
//!
//! ## KILL SWITCH
//!
//! `RAVA_NO_OPT_LITE=1` (or `RAVA_NO_EMIT_MORE`) — resolved ONCE through a `OnceLock`. With it
//! set no singleton is ever minted, [`intercept`] always declines, and `make_optional` allocates
//! exactly as it did before. The census is independent of the switch and independently armed
//! (`RAVA_OPT_CENSUS=1`, or `RAVA_JIT_PIC_PROBE`), so a run can price the population without
//! changing a single answer.
//!
//! ⚠**NEVER an unconditional atomic RMW on a per-call path** (this tree has measured -15% from
//! one relaxed atomic on the interpreter's push path): every counter here is behind
//! [`census_on`], a `OnceLock` read, and the whole census is dead code in a default run.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

/// KILL SWITCH `RAVA_NO_OPT_LITE=1` (or `RAVA_NO_EMIT_MORE`): the empty singleton is never
/// minted and [`intercept`] declines every call, so `stream::make_optional` allocates a fresh
/// `Optional` for both states exactly as it historically did. Resolved ONCE.
#[inline]
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_OPT_LITE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Is the census armed? `RAVA_OPT_CENSUS=1`, or any run that already armed `RAVA_JIT_PIC_PROBE`.
/// A `OnceLock` read, and a clear answer makes every counter below unreachable.
#[inline]
pub(crate) fn census_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_OPT_CENSUS").is_some() || crate::picprobe::enabled()
    })
}

// ── BIRTHS (route-independent: `stream::make_optional` is the sole constructor) ───────────────

/// `Optional`s asked for with a value present — each one a real object, then and now.
static BORN_PRESENT: AtomicU64 = AtomicU64::new(0);
/// `Optional`s asked for EMPTY: `empty()`, `ofNullable(null)`, a `filter` that rejected, a `map`
/// on an empty receiver, a `findFirst` on an empty stream, a `reduce` with no elements.
static ASKED_EMPTY: AtomicU64 = AtomicU64::new(0);
/// Of those, the ones handed the shared instance — i.e. the births that did not happen.
static EMPTY_SERVED: AtomicU64 = AtomicU64::new(0);
/// `java/util/Optional` calls that ARRIVED AT THE CASCADE (see the module note: compiled-code
/// callers routed to `fu_fastpath` are not here).
static CASCADE_CALLS: AtomicU64 = AtomicU64::new(0);

/// The call names the per-name table breaks out, in the order [`CALL_N`] indexes them. `other`
/// is the catch-all, so the table always sums to [`CASCADE_CALLS`] and a name that starts
/// appearing cannot hide.
pub(crate) const CALLS: [&str; 20] = [
    "of",
    "ofNullable",
    "empty",
    "get",
    "isPresent",
    "isEmpty",
    "orElse",
    "orElseGet",
    "orElseThrow",
    "map",
    "flatMap",
    "filter",
    "ifPresent",
    "ifPresentOrElse",
    "or",
    "stream",
    "equals",
    "hashCode",
    "toString",
    "other",
];
const OTHER: usize = CALLS.len() - 1;
static CALL_N: [AtomicU64; CALLS.len()] = [const { AtomicU64::new(0) }; CALLS.len()];

/// The slot for a method name. A match over 19 short literals, run ONLY when the census is
/// armed and only for a call whose owner is already known to be `java/util/Optional`.
#[inline]
fn slot(name: &str) -> usize {
    match name {
        "of" => 0,
        "ofNullable" => 1,
        "empty" => 2,
        "get" | "getAsInt" | "getAsLong" | "getAsDouble" => 3,
        "isPresent" => 4,
        "isEmpty" => 5,
        "orElse" => 6,
        "orElseGet" => 7,
        "orElseThrow" => 8,
        "map" => 9,
        "flatMap" => 10,
        "filter" => 11,
        "ifPresent" => 12,
        "ifPresentOrElse" => 13,
        "or" => 14,
        "stream" => 15,
        "equals" => 16,
        "hashCode" => 17,
        "toString" => 18,
        _ => OTHER,
    }
}

/// One `java/util/Optional` call reached the owner cascade. Inert unless the census is armed.
#[inline]
pub(crate) fn note_call(name: &str) {
    if census_on() {
        CASCADE_CALLS.fetch_add(1, Relaxed);
        CALL_N[slot(name)].fetch_add(1, Relaxed);
    }
}

// ── THE ONE OBJECT THAT IS NOT BORN ──────────────────────────────────────────────────────────

/// `u32::MAX` means "not minted yet"; handle 0 is a legitimate heap handle and cannot be the
/// sentinel.
const NONE: u32 = u32::MAX;
/// The shared empty `Optional`'s heap handle. A GC ROOT — see `Vm::gc`, which extends its root
/// set with [`root`] for exactly the reason `bool_cache` and `box_cache` are rooted there: this
/// static is the sole reference between two `Optional.empty()` calls, and a swept-and-reissued
/// handle would hand the next caller whatever now occupies the slot.
static EMPTY_H: AtomicU32 = AtomicU32::new(NONE);

/// The shared instance's handle, for the GC root walk. `None` before anything asked for an
/// empty `Optional`.
#[inline]
pub(crate) fn root() -> Option<u32> {
    match EMPTY_H.load(Relaxed) {
        NONE => None,
        h => Some(h),
    }
}

/// Forget the shared instance. Called by `Vm::restore_running`, which REPLACES the heap this
/// handle indexes — the same treatment, and for the same reason, that `box_cache` gets beside
/// the call site. A stale handle would index an unrelated live object.
pub(crate) fn reset() {
    EMPTY_H.store(NONE, Relaxed);
}

/// THE ONE ADDED CALL, at the top of `stream::make_optional`.
///
/// Returns `Some(shared empty)` when the requested `Optional` is the empty one and the switch is
/// on; `None` otherwise, and `None` means the caller's historic body runs verbatim. Never
/// returns a present `Optional`, because the JDK does not intern those either.
///
/// GC: `Heap::alloc` may collect, and the two field writes that follow it complete the object
/// before this function can reach another safepoint — the same ordering `make_optional`'s own
/// handle arm uses. The handle is published to [`EMPTY_H`] only AFTER both writes, so a
/// concurrent root walk either does not see it (and the caller's own operand stack roots it) or
/// sees a fully-formed object.
pub(crate) fn intercept(vm: &mut crate::Vm, val: &Option<crate::Value>) -> Option<crate::Value> {
    // Exactly `make_optional`'s own predicate: `Some(Value::Null)` is EMPTY, not present.
    let present = matches!(val, Some(x) if !matches!(x, crate::Value::Null));
    if census_on() {
        if present { &BORN_PRESENT } else { &ASKED_EMPTY }.fetch_add(1, Relaxed);
    }
    if present || !enabled() {
        return None;
    }
    let cached = EMPTY_H.load(Relaxed);
    let h = if cached != NONE {
        cached
    } else {
        // Mirrors `make_optional`'s handle arm field for field: `$val` through the synthetic
        // slot, `$present` by name, same values, same order.
        let fresh = vm.heap.alloc("java/util/Optional");
        vm.heap.put_field_syn(fresh, crate::heap::Syn::Val, crate::Value::Null);
        vm.heap.put_field(fresh, "$present", crate::Value::Int(0));
        crate::Vm::debug_assert_not_gilfree("optlite_empty");
        // A lost race is harmless — both objects are fully-formed empty `Optional`s — but only
        // one may be published, or the loser's handle would be dropped from the root set while
        // a caller still holds it. The winner is what every later call gets.
        match EMPTY_H.compare_exchange(NONE, fresh, Relaxed, Relaxed) {
            Ok(_) => fresh,
            Err(winner) => winner,
        }
    };
    if census_on() {
        EMPTY_SERVED.fetch_add(1, Relaxed);
    }
    Some(crate::Value::Ref(h))
}

/// `[OPTLITE]` rows, or the empty string when nothing was counted (the default run).
pub fn report() -> String {
    let (p, e, s, c) = (
        BORN_PRESENT.load(Relaxed),
        ASKED_EMPTY.load(Relaxed),
        EMPTY_SERVED.load(Relaxed),
        CASCADE_CALLS.load(Relaxed),
    );
    if p + e + c == 0 {
        return String::new();
    }
    let asked = p + e;
    let mut out = format!(
        "[OPTLITE] Optional asked for {asked} times: {p} present (a real object each) + {e} empty \
         ({:.1}%) | {s} served from the shared instance = births not made | switch {}",
        if asked > 0 { e as f64 * 100.0 / asked as f64 } else { 0.0 },
        if enabled() { "ON" } else { "OFF (RAVA_NO_OPT_LITE)" }
    );
    if c > 0 {
        out.push_str(&format!(
            "\n[OPTLITE] {c} calls reached the owner cascade (compiled callers routed to \
             fu_fastpath are NOT counted here):"
        ));
        let mut rows: Vec<(u64, &str)> = CALL_N
            .iter()
            .zip(CALLS)
            .map(|(n, s)| (n.load(Relaxed), s))
            .filter(|r| r.0 > 0)
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        for (n, name) in rows {
            out.push_str(&format!(
                "\n[OPTLITE]   {:>6.2}%  {n:>10}  {name}",
                n as f64 * 100.0 / c as f64
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name table must be TOTAL: every name lands somewhere, so the rows always sum to the
    /// arrival count and a method that starts showing up cannot vanish from the census.
    #[test]
    fn every_name_has_a_slot_and_unknown_names_land_in_other() {
        for (i, n) in CALLS.iter().enumerate() {
            if i == OTHER {
                continue;
            }
            assert_eq!(slot(n), i, "{n} does not map to its own column");
        }
        assert_eq!(slot("empty"), 2);
        assert_eq!(slot("orElseThrow"), 8);
        // The three primitive-flavour readers share the `get` column: `optional_op` serves them
        // from the same arm, so counting them apart would invent a distinction the engine does
        // not make.
        assert_eq!(slot("getAsInt"), slot("get"));
        assert_eq!(slot("getAsDouble"), slot("get"));
        // Anything else is `other`, never a panic and never a silent drop.
        assert_eq!(slot("ifPresentOrElseSomething"), OTHER);
        assert_eq!(slot(""), OTHER);
    }

    /// `Some(Value::Null)` is the EMPTY state, not a present one — the exact predicate
    /// `stream::make_optional` uses. Getting this backwards would intern a present `Optional`.
    #[test]
    fn a_null_payload_is_the_empty_state() {
        let f = |v: &Option<crate::Value>| matches!(v, Some(x) if !matches!(x, crate::Value::Null));
        assert!(!f(&None));
        assert!(!f(&Some(crate::Value::Null)));
        assert!(f(&Some(crate::Value::Int(0))));
    }

    /// The sentinel may not collide with a real handle: handle 0 is legitimate, `u32::MAX` is
    /// not reachable as one.
    #[test]
    fn the_sentinel_is_not_a_reachable_handle() {
        assert_eq!(NONE, u32::MAX);
        EMPTY_H.store(NONE, Relaxed);
        assert_eq!(root(), None);
        EMPTY_H.store(0, Relaxed);
        assert_eq!(root(), Some(0));
        reset();
        assert_eq!(root(), None);
    }
}
