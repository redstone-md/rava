//! THE MAP INDEX'S TABLE OPERATIONS — the renumbering that `heap::maps` does around
//! `MapObj::entries`, with the per-operation work taken out of it.
//!
//! A guest map is two structures: `entries: Vec<(Value, Value)>` IS the iteration order
//! (java's `LinkedHashMap`/`TreeMap` orders are specified, and this engine's five bit-exact
//! hash gates hash real output, so the order `entries` produces is not negotiable), and
//! `index: FastMap<MapKey, usize>` is a LOOKUP ACCELERATOR whose values are POSITIONS in
//! that vector. Everything in this file exists because those positions are dense: move or
//! delete one entry and every position after it shifts by one.
//!
//! The shape that was there: every order-changing op walked the WHOLE hash table
//! (`index.values_mut()`) to renumber. That walk touches every bucket the table has
//! allocated, empty ones included — a 4096-entry map with a 8192-bucket table pays 8192
//! bucket visits to delete ONE key, and it pays them even when the deleted key was the
//! LAST entry and nothing needed renumbering at all. That is the shape of MC's LRU caches
//! (`LinkedHashMap` access-order, fastutil linked maps): every `get` reorders, every
//! reorder was a full-table scan.
//!
//! The fix is arithmetic, not heuristics: for a given `pos` we know EXACTLY how many index
//! slots can possibly change, because positions are dense and unique. Removing entry `pos`
//! from a map of `n` changes exactly `n - 1 - pos` slots; rotating `pos` to the front
//! changes exactly `pos + 1`. Count them down and stop the moment the budget is spent. The
//! result is bit-identical to the full scan — same slots get the same new values — it just
//! stops walking buckets that provably hold nothing left to fix. When the moved entry is
//! already at the end (`pos == n - 1`, the overwhelmingly common delete), the budget is
//! zero and the scan does not start.
//!
//! NO NEW TABLE, NO NEW BYTES. This file adds zero persistent state: the only new storage
//! is one `usize` countdown in a register. Nothing here can push a working set out of L1 —
//! it strictly REMOVES bucket traffic from the paths it serves.
//!
//! SOUNDNESS: `index` is order-independent by construction (see `MapObj::index`), so which
//! buckets a scan visits, and in what order, is not guest-visible. Only the (position →
//! entry) mapping is, and that is what these helpers preserve exactly.
//!
//! Kill switch: `RAVA_NO_FAST_MAPTABLE=1` (or `RAVA_NO_EMIT_MORE=1`) restores the
//! unbudgeted full-table walk on every path, so the change prices as a same-binary A/B.

use crate::heap::MapKey;
use crate::FastMap;

pub(crate) type Index = FastMap<MapKey, usize>;

/// DEFAULT ON. Off ⇒ every helper below degenerates to the original full `values_mut()`
/// scan (identical results, more bucket visits).
#[inline]
pub(crate) fn fast_maptable_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::fast_natives() && std::env::var_os("RAVA_NO_FAST_MAPTABLE").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

/// Entry `pos` was just removed from a map that held `n_before` entries: every position
/// strictly above `pos` slides down by one. There are exactly `n_before - 1 - pos` of them.
///
/// `pos == n_before - 1` (deleting the newest entry — what `remove`-after-`put`, single-entry
/// maps and every LIFO use of a map does) needs ZERO fixes, so the table is not touched at all.
#[inline]
pub(crate) fn shift_down_after(index: &mut Index, pos: usize, n_before: usize) {
    if !fast_maptable_enabled() {
        for slot in index.values_mut() {
            if *slot > pos {
                *slot -= 1;
            }
        }
        return;
    }
    let mut budget = n_before.saturating_sub(pos + 1);
    if budget == 0 {
        return;
    }
    for slot in index.values_mut() {
        if *slot > pos {
            *slot -= 1;
            budget -= 1;
            if budget == 0 {
                return;
            }
        }
    }
}

/// Entry `pos` was rotated to the FRONT: positions below it slide up by one, and it becomes 0.
/// Exactly `pos + 1` slots change, so a hit on the newest entry of a 4096-entry LRU stops after
/// finding one bucket instead of visiting eight thousand.
///
/// Replaces a `index.insert(key.clone(), 0)` that followed the old scan: `clone()` on a
/// `MapKey::Str` ALLOCATES a `Box<str>` copy of a key the table already owns, once per LRU
/// touch, and the insert re-hashed and re-probed for a slot the scan had just walked past.
#[inline]
pub(crate) fn rotate_to_front(index: &mut Index, pos: usize) {
    let mut budget = pos + 1;
    if !fast_maptable_enabled() {
        for slot in index.values_mut() {
            if *slot < pos {
                *slot += 1;
            } else if *slot == pos {
                *slot = 0;
            }
        }
        return;
    }
    for slot in index.values_mut() {
        if *slot < pos {
            *slot += 1;
        } else if *slot == pos {
            *slot = 0;
        } else {
            continue;
        }
        budget -= 1;
        if budget == 0 {
            return;
        }
    }
}

/// Entry `pos` was rotated to the BACK of a map of `n` entries: positions above it slide down
/// by one, and it becomes `n - 1`. Exactly `n - pos` slots change.
///
/// This is the access-ordered `LinkedHashMap` hit — java moves the entry to the END on `get`.
/// The path it replaces was `map_remove` + `map_put`: a full-table renumbering scan, a
/// `Vec::remove` memmove, a `Vec::push`, a second hash+probe to delete the key and a THIRD to
/// re-insert it, plus a second derivation of the `MapKey` itself. One scan and one rotate here.
#[inline]
pub(crate) fn rotate_to_back(index: &mut Index, pos: usize, n: usize) {
    let last = n - 1;
    let mut budget = n - pos;
    if !fast_maptable_enabled() {
        for slot in index.values_mut() {
            if *slot > pos {
                *slot -= 1;
            } else if *slot == pos {
                *slot = last;
            }
        }
        return;
    }
    for slot in index.values_mut() {
        if *slot > pos {
            *slot -= 1;
        } else if *slot == pos {
            *slot = last;
        } else {
            continue;
        }
        budget -= 1;
        if budget == 0 {
            return;
        }
    }
}
