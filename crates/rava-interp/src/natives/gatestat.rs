//! Census helpers on the dispatch gate and the arena-map receiver test.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// Whether a pattern is a plain literal — no regex metacharacters — so it can be
/// matched/split with raw `str` ops and skip the regex engine entirely. Note `.`
/// is excluded: Java's `split(".")` means "any char", not a literal dot.
pub(crate) fn regex_is_literal(p: &str) -> bool {
    !p.is_empty()
        && !p.bytes().any(|b| {
            matches!(
                b,
                b'\\' | b'.' | b'[' | b']' | b'(' | b')' | b'{' | b'}'
                    | b'*' | b'+' | b'?' | b'|' | b'^' | b'$'
            )
        })
}

pub(crate) fn gate_census_note(owner: &str, name: &str) {
    if let Ok(mut t) = gate_census().lock() {
        let e = t
            .entry((owner.as_ptr() as usize, name.as_ptr() as usize))
            .or_insert_with(|| (format!("{owner}#{name}"), 0));
        e.1 += 1;
    }
}

/// `map::dispatch` serves an arena-map receiver ahead of every owner test, so that
/// receiver shape must enter it whatever the owner. Inert unless RAVA_ARENA_MAPS.
#[inline]
pub(crate) fn arena_map_receiver(args: &[Value]) -> bool {
    #[cfg(feature = "next-gen-gc")]
    {
        return crate::arena_map::enabled() && matches!(args.first(), Some(Value::Obj(_)));
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let _ = args;
        false
    }
}
