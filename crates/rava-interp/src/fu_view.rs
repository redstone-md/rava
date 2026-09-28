//! Field-identity memo for the fastutil scalar fast path (`RAVA_FU_FIELD_IDS`).
//!
//! `Vm::fu_double_view` / `Vm::fu_int_view` project a fastutil primitive list onto
//! `(backing array handle, logical length)`. To do that they need two field
//! identities — the backing array (fastutil always names it `a`) and, for the
//! over-allocated ARRAY-list flavours, the logical length (`size`). Both were
//! re-derived BY NAME on every single fast-path call.
//!
//! WHY THAT COSTS. `Vm::resolve_field_id` answers from `tlcache::field_key`, but a
//! HIT there still costs a thread-local access plus a hash of a 44-character class
//! name and a field name, to read back a `u32` that can never change. RIP sampling
//! of the real boot (docs/rip-profile-2026-07-27.md, ring window) puts one chain —
//! `resolve_field_id < fu_double_view < jit_fu_invoke < rava_fu_invoke` — at 6.73%
//! of the ring, and it is the ONLY chain under `resolve_field_id` in that window
//! (396/396 samples). It is why `LocalKey::with` is the ring's #2 leaf at 8.9% with
//! 68% of its proven consumers being `resolve_field_id`. `PerlinNoise.getValue`
//! reads `this.amplitudes.getDouble(i)` once per octave and `amplitudes` is a
//! `DoubleImmutableList`, so this is one name lookup per octave per sample.
//!
//! THE SHAPE IS ALREADY IN THE FILE. `Vm::fu_iter_field_ids` memoises the native
//! iterator's `($elems, $pos)` identities in a `OnceLock` for exactly this reason;
//! the view functions were simply never given the same treatment.
//!
//! WHY A PROCESS-WIDE CELL IS SOUND. A field identity is a pure function of
//! immutable post-boot loader state composed with the idempotent field interner —
//! the very argument the per-thread `tlcache::field_key` memo already documents
//! ("same name → same id on any thread"). This hoists that memo from per-thread to
//! per-process for a fixed, closed set of library classes. Identities are interner
//! ids, NOT addresses or handles, so neither `GC_EPOCH` nor `HANDLE_EPOCH` applies.
//! Two threads racing the first resolution compute the same value and the loser's
//! `set` is dropped.
//!
//! ONE CELL PER (CLASS, FIELD), NOT ONE PER FAMILY. `DoubleImmutableList` and
//! `DoubleArrayList` are unrelated fastutil classes that each declare their own `a`,
//! so their identities differ — a single cell would serve one class's id to the
//! other. [`FamilyIds`] therefore holds the three cells a list family actually uses
//! and hands them out by flavour. Keeping them separate also preserves the exact
//! LAZINESS of the original code: `size` is still resolved only after the backing
//! array has been read and only for an array list, so the order in which field
//! names first reach the interner is unchanged.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// One memoised field identity. `None` until the first resolution publishes it.
pub(crate) struct FieldIdCell(OnceLock<u32>);

impl FieldIdCell {
    pub(crate) const fn new() -> Self {
        Self(OnceLock::new())
    }
    pub(crate) fn get(&self) -> Option<u32> {
        self.0.get().copied()
    }
    /// Publish. A concurrent first-resolver that lost the race computed the same
    /// id (pure function of loader state), so dropping this write is harmless.
    pub(crate) fn set(&self, id: u32) {
        let _ = self.0.set(id);
    }
}

/// The field identities ONE fastutil primitive-list family needs: the backing
/// array of each flavour, plus the array flavour's logical length. An immutable
/// list declares no `size` (its length IS the array's), so it has no cell.
pub(crate) struct FamilyIds {
    imm_a: FieldIdCell,
    arr_a: FieldIdCell,
    arr_size: FieldIdCell,
}

impl FamilyIds {
    pub(crate) const fn new() -> Self {
        Self { imm_a: FieldIdCell::new(), arr_a: FieldIdCell::new(), arr_size: FieldIdCell::new() }
    }
    /// The backing-array (`a`) cell of the requested flavour.
    pub(crate) fn a(&self, is_arraylist: bool) -> &FieldIdCell {
        if is_arraylist {
            &self.arr_a
        } else {
            &self.imm_a
        }
    }
    /// The array flavour's logical-length (`size`) cell.
    pub(crate) fn size(&self) -> &FieldIdCell {
        &self.arr_size
    }
}

/// Serve the fast path's field identities from the per-class memo instead of
/// re-resolving them by name on every call.
///
/// DEFAULT-ON since 2026-07-27 (`RAVA_NO_FU_FIELD_IDS` disables), on the Done window
/// where the lever lives: six interleaved full boots, P-cores, quiet machine,
///   OFF  49.272 / 47.225 / 47.183 s        ON  45.838 / 42.885 / 42.947 s
/// EVERY ON below EVERY OFF, min-of-N **-9.11%**. The RIP profile predicted 14.0% of
/// the ring for the whole `rava_fu_invoke` chain and 6.7% for `resolve_field_id`
/// inside it, so the measured 9.1% of the Done window sits exactly where a fix to
/// this chain should land - mechanism and magnitude agree rather than merely
/// pointing the same way.
pub(crate) fn memo_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FU_FIELD_IDS").is_none())
}

/// Engagement probe (`RAVA_FU_FIELD_IDS_PROBE`): counts, which are immune to
/// machine contention in a way walls are not. With the memo OFF every fast-path
/// call is a `name_lookups` tick; with it ON `name_lookups` must stop at the
/// number of distinct (class, field) pairs the run touches — at most 6 — while
/// `memo_hits` tracks the call count. The ratio between the two arms IS the
/// engagement proof.
fn probe_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FU_FIELD_IDS_PROBE").is_some())
}

static NAME_LOOKUPS: AtomicU64 = AtomicU64::new(0);
static MEMO_HITS: AtomicU64 = AtomicU64::new(0);

/// Report on power-of-two crossings only: bounded output (~30 lines for 10^9
/// calls), dense while the memo warms and sparse once it has, and every line
/// carries both cumulative totals so any one of them gives the ratio.
fn note(counter: &AtomicU64) {
    if !probe_enabled() {
        return;
    }
    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_power_of_two() {
        eprintln!(
            "[FU-FIELD-IDS] memo={} name_lookups={} memo_hits={}",
            memo_enabled(),
            NAME_LOOKUPS.load(Ordering::Relaxed),
            MEMO_HITS.load(Ordering::Relaxed),
        );
    }
}

pub(crate) fn note_name_lookup() {
    note(&NAME_LOOKUPS);
}
pub(crate) fn note_memo_hit() {
    note(&MEMO_HITS);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A family hands out a DISTINCT cell per (flavour, field) — the property that
    /// stops `DoubleArrayList`'s `a` being served to a `DoubleImmutableList`.
    #[test]
    fn family_cells_are_distinct() {
        static F: FamilyIds = FamilyIds::new();
        F.a(false).set(11);
        F.a(true).set(22);
        F.size().set(33);
        assert_eq!(F.a(false).get(), Some(11));
        assert_eq!(F.a(true).get(), Some(22));
        assert_eq!(F.size().get(), Some(33));
    }

    /// First write wins and later writes are dropped, so a lost publish race can
    /// never overwrite an id another thread is already using.
    #[test]
    fn cell_publishes_once() {
        let c = FieldIdCell::new();
        assert_eq!(c.get(), None);
        c.set(7);
        c.set(9);
        assert_eq!(c.get(), Some(7));
    }
}
