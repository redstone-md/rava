//! ⭐**A BOX MUST CARRY ITS OWN TYPE.** `Character`, `Short` and `Byte`, and the one gate,
//! one cache and one census they share.
//!
//! ## THE DEFECT THIS MODULE EXISTS TO CLOSE
//!
//! This engine keeps primitives unboxed, and `Value::Int` is the representation of a boxed
//! `Integer`. For `Integer`/`Long`/`Float`/`Double` that is harmless — the `Value` variant
//! already NAMES the type and renders exactly as Java renders the box. For the three narrow
//! boxes it erases the type, and the erasure is observable in four different ways:
//!
//! ```text
//! java:  java.lang.Character |#| valueOf=# concat=#      map {#=1, x=2}    list [#, true, 5]
//! rava:  java.lang.Integer   |35| valueOf=35 concat=35   map {35=1, 120=2} list [35, true, 5]
//! ```
//!
//! and it is not a probe curiosity: a shaped Minecraft recipe's `key` is a
//! `Map<Character, Ingredient>`, so `Recipe.CODEC` encoded `{"key":{"35":"minecraft:paper"}}`
//! where the JVM writes `{"key":{"#":"minecraft:paper"}}` (bench/CompSem.java `recipe.encode`,
//! and the `HASH` of bench/ReloadBench.java with it). `Short.valueOf`/`Byte.valueOf` have the
//! same defect, invisible in rendering because it is numeric, but wrong for `getClass()`,
//! `instanceof`, `equals` against a real `Short`/`Byte`, and any map keyed by the box.
//!
//! ## THE SHAPE OF THE FIX, AND WHOSE PATTERN IT IS
//!
//! `java/lang/Boolean` had this exact bug and was fixed by boxing into a REAL heap object whose
//! `$value` field carries the payload ([`crate::Vm::boolean_obj`]), with an unboxing helper
//! (`crate::natives::bool_of`) at every consumer that reads the payload. This module is that
//! pattern applied to the other three, with the JDK's own interning ranges so `valueOf`
//! IDENTITY matches the reference implementation: `Character` 0..=127, `Short` and `Byte`
//! -128..=127 (JLS 5.1.7).
//!
//! ⚠**THE DANGEROUS HALF IS UNBOXING COVERAGE, NOT BOXING.** Every consumer that used to
//! receive `Value::Int(35)` now receives a `Value::Ref`, and `Value::as_int` answers `None` for
//! a `Ref` — so a missed consumer reads `unwrap_or(0)`, silently. [`int_of`] is the one helper
//! they all take, and the four choke points that make the coverage structural rather than
//! enumerated are:
//!
//! 1. `argcall::expand_locals_into` — a `C`/`S`/`B` PARAMETER receiving a box (the `Z` rule that
//!    already stood there, widened).
//! 2. `Vm::invoke` — a `)C`/`)S`/`)B` RETURN handing a box back to an `istore`/`ifeq`.
//! 3. `natives::cascade_table` — every arm keyed on one of the three owners.
//! 4. `map::hashing` / `map::keys` — the by-value hash and the by-value map key, so a box is
//!    the same key its payload was.
//!
//! ## KILL SWITCH
//!
//! `RAVA_NO_BOXID=1` (or `RAVA_NO_EMIT_MORE`) restores the historic identity `valueOf` EXACTLY
//! — see [`enabled`]. Resolved once through a `OnceLock`. The unboxing helpers stay live in both
//! arms: with the switch on no box is ever minted, so they never fire, and a live unboxer cannot
//! change an answer it is never handed.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

/// The three narrow boxes, in the order [`BoxCache`] and [`CENSUS`] index them.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Char = 0,
    Short = 1,
    Byte = 2,
}

impl Kind {
    /// The internal class name this kind boxes into.
    pub(crate) const fn class(self) -> &'static str {
        match self {
            Kind::Char => "java/lang/Character",
            Kind::Short => "java/lang/Short",
            Kind::Byte => "java/lang/Byte",
        }
    }

    /// The kind that `class` boxes, if any. `java/lang/Boolean` is deliberately ABSENT: it has
    /// its own cache and its own `bool_of`, and folding it in here would give one payload two
    /// owners.
    pub(crate) fn of_class(class: &str) -> Option<Kind> {
        match class {
            "java/lang/Character" => Some(Kind::Char),
            "java/lang/Short" => Some(Kind::Short),
            "java/lang/Byte" => Some(Kind::Byte),
            _ => None,
        }
    }

    /// Narrow `n` the way the primitive store would, so the box's payload is the value a
    /// `c2i`/`i2s`/`i2b` conversion would have left on the stack.
    pub(crate) fn narrow(self, n: i32) -> i32 {
        match self {
            Kind::Char => n as u16 as i32,
            Kind::Short => n as i16 as i32,
            Kind::Byte => n as i8 as i32,
        }
    }
}

/// ⭐**THE JDK'S OWN INTERNING RANGES, so `valueOf` IDENTITY is the reference implementation's.**
///
/// `Character.valueOf` caches 0..=127 and `Short.valueOf`/`Byte.valueOf` cache -128..=127 (JLS
/// 5.1.7 requires it for the latter two). Guest code compares boxes with `==` — and, more to the
/// point for this engine, `map::keys` falls back to an IDENTITY key for a class it cannot render
/// by value, so a cache that disagreed with the JDK's would make `m.put(c, v); m.get(c)` depend
/// on which side of the range `c` sat. The value-based key arm in `map::keys` covers the rest, so
/// the cache is an identity and allocation matter, never a correctness one.
///
/// 640 `Option<u32>` = 5 KB, held behind a `Box` so `Vm` does not grow by it. Every live handle
/// is a GC root (`Vm::gc`).
pub(crate) struct BoxCache {
    /// `Character.valueOf` 0..=127.
    pub(crate) chars: [Option<u32>; 128],
    /// `Short.valueOf` -128..=127, indexed `n + 128`.
    pub(crate) shorts: [Option<u32>; 256],
    /// `Byte.valueOf` -128..=127, indexed `n + 128`.
    pub(crate) bytes: [Option<u32>; 256],
}

impl Default for BoxCache {
    fn default() -> Self {
        BoxCache { chars: [None; 128], shorts: [None; 256], bytes: [None; 256] }
    }
}

impl BoxCache {
    /// The cache slot for `(kind, n)`, or `None` when `n` is outside the JDK's range for it.
    /// `n` must already be NARROWED (see [`Kind::narrow`]) — a raw `i32` would index the wrong
    /// slot for a `Short` that arrived unmasked.
    pub(crate) fn slot(&self, kind: Kind, n: i32) -> Option<usize> {
        match kind {
            Kind::Char => (0..=127).contains(&n).then_some(n as usize),
            Kind::Short | Kind::Byte => (-128..=127).contains(&n).then(|| (n + 128) as usize),
        }
    }

    /// The interned handle for `(kind, n)`, if one has been minted.
    pub(crate) fn get(&self, kind: Kind, n: i32) -> Option<u32> {
        let i = self.slot(kind, n)?;
        match kind {
            Kind::Char => self.chars[i],
            Kind::Short => self.shorts[i],
            Kind::Byte => self.bytes[i],
        }
    }

    /// Intern `h` as the box for `(kind, n)`. A no-op outside the range, so an out-of-range box
    /// is a fresh object every time — exactly as the JDK's is.
    pub(crate) fn put(&mut self, kind: Kind, n: i32, h: u32) {
        if let Some(i) = self.slot(kind, n) {
            match kind {
                Kind::Char => self.chars[i] = Some(h),
                Kind::Short => self.shorts[i] = Some(h),
                Kind::Byte => self.bytes[i] = Some(h),
            }
        }
    }

    /// Every live interned handle, for the GC root walk.
    pub(crate) fn handles(&self) -> impl Iterator<Item = u32> + '_ {
        self.chars
            .iter()
            .chain(self.shorts.iter())
            .chain(self.bytes.iter())
            .filter_map(|s| *s)
    }
}

/// KILL SWITCH `RAVA_NO_BOXID=1` (or `RAVA_NO_EMIT_MORE`): `Character.valueOf` / `Short.valueOf`
/// / `Byte.valueOf` return `Some(args.first().cloned())` — the historic identity — and no box is
/// ever minted, so every unboxing site added for this work is unreachable and the engine answers
/// exactly as it did before. Resolved ONCE; the guard may only REJECT.
#[inline]
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_BOXID").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `[BOXID]` — boxes MINTED and boxes UNBOXED, per kind, plus the interning hit rate.
///
/// ⚠**NEVER an unconditional atomic RMW on a per-call path.** `valueOf` is a per-autobox event
/// and `int_of` runs on every narrow-box consumer, and this tree has measured a **−15%** swing
/// from ONE relaxed atomic on the interpreter's push path — an atomic's cost is that LLVM cannot
/// HOIST it, not the `mov`. So every counter below is behind `picprobe::enabled()`, read from a
/// `OnceLock`, and the whole census is dead code in a default run. Reported from the periodic
/// profiler dump (`pc_profile/dump.rs`), beside `compile_bail::report` — never from a teardown a
/// server boot exits by `System.exit` and therefore never reaches.
///
/// Layout: `[minted, interned, unboxed]` per kind, indexed by [`Kind`].
static CENSUS: [[AtomicU64; 3]; 3] = [const { [const { AtomicU64::new(0) }; 3] }; 3];

const MINTED: usize = 0;
const INTERNED: usize = 1;
const UNBOXED: usize = 2;

/// A box was handed out for `kind`; `interned` iff it came from (or entered) the cache.
#[inline]
pub(crate) fn note_mint(kind: Kind, interned: bool) {
    if crate::picprobe::enabled() {
        CENSUS[kind as usize][MINTED].fetch_add(1, Relaxed);
        if interned {
            CENSUS[kind as usize][INTERNED].fetch_add(1, Relaxed);
        }
    }
}

/// A consumer read the payload back out of a box of `kind`.
#[inline]
pub(crate) fn note_unbox(kind: Kind) {
    if crate::picprobe::enabled() {
        CENSUS[kind as usize][UNBOXED].fetch_add(1, Relaxed);
    }
}

/// `[BOXID]` rows, or the empty string when nothing was counted (the default run).
pub(crate) fn report() -> String {
    let total: u64 =
        CENSUS.iter().flat_map(|k| k.iter()).map(|c| c.load(Relaxed)).sum();
    if total == 0 {
        return String::new();
    }
    let mut out = String::from("[BOXID] narrow-box identity (RAVA_NO_BOXID=1 disables)");
    for (i, kind) in [Kind::Char, Kind::Short, Kind::Byte].into_iter().enumerate() {
        let (m, it, u) = (
            CENSUS[i][MINTED].load(Relaxed),
            CENSUS[i][INTERNED].load(Relaxed),
            CENSUS[i][UNBOXED].load(Relaxed),
        );
        if m + u == 0 {
            continue;
        }
        let pct = if m == 0 { 0.0 } else { it as f64 * 100.0 / m as f64 };
        out.push_str(&format!(
            "\n[BOXID]   {:<20} minted {m:>10}  interned {it:>10} ({pct:>5.1}%)  unboxed {u:>10}",
            kind.class()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interning ranges ARE the JDK's, and the boundary is the whole point: a cache that
    /// stopped one short of 127 would hand out a fresh object for a char guest code compares
    /// with `==`.
    #[test]
    fn interning_ranges_are_the_jdk_s() {
        let c = BoxCache::default();
        assert_eq!(c.slot(Kind::Char, 0), Some(0));
        assert_eq!(c.slot(Kind::Char, 127), Some(127));
        assert_eq!(c.slot(Kind::Char, 128), None);
        assert_eq!(c.slot(Kind::Char, -1), None);
        for k in [Kind::Short, Kind::Byte] {
            assert_eq!(c.slot(k, -128), Some(0));
            assert_eq!(c.slot(k, 0), Some(128));
            assert_eq!(c.slot(k, 127), Some(255));
            assert_eq!(c.slot(k, 128), None);
            assert_eq!(c.slot(k, -129), None);
        }
    }

    /// A box's payload is the NARROWED value, so `(byte) 200` boxes to -56 and keys, hashes and
    /// renders as -56 — the number the JVM's `i2b` would have left on the stack.
    #[test]
    fn narrowing_matches_the_primitive_conversions() {
        assert_eq!(Kind::Char.narrow(65_601), 65); // 65601 & 0xFFFF == 65 ('A')
        assert_eq!(Kind::Char.narrow(-1), 65535);
        assert_eq!(Kind::Short.narrow(40_000), 40_000 - 65_536);
        assert_eq!(Kind::Byte.narrow(200), -56);
        assert_eq!(Kind::Byte.narrow(3), 3);
    }

    /// Round trip through the cache, and the out-of-range slot that must stay uninterned.
    #[test]
    fn the_cache_interns_in_range_and_refuses_outside_it() {
        let mut c = BoxCache::default();
        c.put(Kind::Char, 35, 7);
        assert_eq!(c.get(Kind::Char, 35), Some(7));
        assert_eq!(c.get(Kind::Char, 36), None);
        c.put(Kind::Char, 4000, 9);
        assert_eq!(c.get(Kind::Char, 4000), None);
        c.put(Kind::Byte, -128, 11);
        c.put(Kind::Short, 127, 12);
        assert_eq!(c.get(Kind::Byte, -128), Some(11));
        assert_eq!(c.get(Kind::Short, 127), Some(12));
        // Three kinds, three independent tables: a Byte 3 is not a Short 3 is not a Char 3.
        c.put(Kind::Byte, 3, 21);
        assert_eq!(c.get(Kind::Short, 3), None);
        assert_eq!(c.get(Kind::Char, 3), None);
        let mut hs: Vec<u32> = c.handles().collect();
        hs.sort_unstable();
        assert_eq!(hs, vec![7, 11, 12, 21]);
    }

    /// Every class in `of_class` round-trips to itself, and `Boolean` is NOT one of them.
    #[test]
    fn class_mapping_round_trips_and_excludes_boolean() {
        for k in [Kind::Char, Kind::Short, Kind::Byte] {
            assert_eq!(Kind::of_class(k.class()), Some(k));
        }
        assert_eq!(Kind::of_class("java/lang/Boolean"), None);
        assert_eq!(Kind::of_class("java/lang/Integer"), None);
    }
}
