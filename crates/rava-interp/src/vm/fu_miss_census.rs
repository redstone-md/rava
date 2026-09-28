//! FU-MISS RECEIVER CENSUS (`RAVA_FU_MISS_CENSUS=1`, default OFF; `RAVA_NO_FU_MISS_CENSUS=1`
//! withdraws, `RAVA_NO_EMIT_MORE=1` blocks) — observe-only, never routes.
//!
//! `fu_missed` is the engine's largest unattributed residue: **12.06 M refusals a boot — 40.2%
//! of the 30 M `fu_fastpath` attempts** — every one a crossing that paid the whole attempt and
//! still owed the general dispatch, and the counter has never been split by RECEIVER CLASS.
//! This census is that split. The hook sits in `jit_fu_invoke`'s `if diag` block beside the
//! counter's ONE increment site, so the census total equals `fu_missed` in the same run; the
//! dump header prints both and a MATCH/MISMATCH verdict.
//!
//! Same discipline as `choke_residue`: keyed by integers only — the receiver class key
//! `fu_kind_of` memoises (heap class word / `(arena_class_id << 1) | 1`), an FNV-1a of the
//! member name, and the site `(caller class ptr, cp)` — no String at note time (the
//! O(string)-per-call defect class); symbols and receiver names are resolved ONCE at dump.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Kill switch (`RAVA_NO_FU_MISS_CENSUS=1`), then the block switch, then the arming flag —
/// so `RAVA_NO_EMIT_MORE` bisects this census out together with the rest of the block in
/// one run.
static ON: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("RAVA_NO_FU_MISS_CENSUS").is_none()
        && !crate::flags::emit_more_disabled()
        && std::env::var_os("RAVA_FU_MISS_CENSUS").is_some()
});

pub(crate) fn enabled() -> bool {
    *ON
}

/// `(receiver class key, member-name FNV, caller LoadedClass ptr, cp)` → counts. The class
/// ptr is stable for the process lifetime (the loader never unloads), so it is a safe
/// dump-time key — `fallback_tally`'s and `choke_residue`'s key, with the RECEIVER split
/// this census exists to make.
static TALLY: Mutex<Option<HashMap<(u64, u64, usize, u16), Row>>> = Mutex::new(None);

/// One (class, member, site) row's counts, split by the receiver's arrival representation
/// from the cell's TAG BITS ONLY: `rep[0]` heap handle / `rep[1]` arena-or-pinned /
/// `rep[2]` null, box, array or anything else.
struct Row {
    rep: [u64; 3],
}

/// Count one `fu_missed` event. `cells` is the raw call buffer; `cells[0]` is the receiver
/// (for the admitted statics, the first argument — the same cell `fu_fastpath` unboxes as
/// its `recv`).
#[inline]
pub(crate) fn note(vm: &crate::Vm, caller_ptr: usize, cp: u16, cells: &[i64]) {
    if !enabled() {
        return;
    }
    // `fu_fastpath` may have allocated, and a Cheney pass can move the receiver, so the
    // cell is read through the same barrier every other post-attempt reader of `raw[0]`
    // applies (`jit_direct_probe`, `jit_call`'s pure-memo key).
    let cell = crate::nextgen::forward_if_moved(cells.first().copied().unwrap_or(0));
    let r = cell as u64;
    let (key, rep) = if r != 0 && r >> 32 == (crate::value::REF_TAG as u64) >> 32 {
        // Heap handle: the class WORD is the integer the kind memo and the emitted guards
        // compare, stable for the process lifetime — a safe dump-time key.
        let h = (r & 0xFFFF_FFFF) as u32;
        (
            vm.heap
                .object_class_word(h)
                .filter(|w| *w as u64 != u64::MAX)
                .map_or(0, |w| w as u64),
            0,
        )
    } else if crate::nextgen::is_arena_obj_addr(r) {
        // Arena object or a pinned mirror (whose header carries the REAL registered class
        // id) — bounded by the same predicate every other raw-cell header read gates on.
        let p = unsafe { crate::arena::ArenaPtr::from_addr(r) };
        (((crate::nextgen::arena_class_id(p) as u64) << 1) | 1, 1)
    } else {
        (0, 2)
    };
    // FNV-1a over the in-scope member name — the pool's own `&str`, hashed where it lies.
    // SAFETY: the loader never unloads, so the caller class outlives the note — the same
    // discipline as `invoke_census::note`.
    let caller = unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    if let Some(m) = caller.file.pool.member_ref(cp) {
        for &b in m.name.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    if let Ok(mut t) = TALLY.lock() {
        t.get_or_insert_with(HashMap::new)
            .entry((key, h, caller_ptr, cp))
            .or_insert_with(|| Row { rep: [0; 3] })
            .rep[rep] += 1;
    }
}

/// Top 40 rows by count, one line each, plus the MATCH self-check against the `fu_missed`
/// counter. The hook notes beside the counter's increment site, so the totals agree by
/// construction on a quiescent dump.
pub(crate) fn dump(vm: &crate::Vm) {
    if !enabled() {
        return;
    }
    // The counter is read BEFORE the TALLY lock, for the same reason `choke_residue::dump`
    // reads its meter cells first: it is one atomic, and holding the note path's lock
    // while walking it buys nothing.
    let ctr = crate::diag::fu_missed();
    let ev = ctr.get();
    let Ok(mut g) = TALLY.lock() else { return };
    // An empty map is a FINDING — "every attempt was served" — not a silent nothing.
    // Always print the header; rows follow only if there are any.
    let map = g.get_or_insert_with(HashMap::new);
    let total: u64 = map.values().map(|r| r.rep.iter().sum::<u64>()).sum();
    let verdict = if !ctr.armed() {
        "counter-off(arm RAVA_DIAG=price.fu_missed for the cross-check)"
    } else if ev == total {
        "MATCH"
    } else {
        "MISMATCH!"
    };
    eprintln!("[FU-MISS] total={total} sites={} fu_missed_ev={ev} {verdict}", map.len());
    let mut rows: Vec<(&(u64, u64, usize, u16), &Row)> = map.iter().collect();
    rows.sort_by(|a, b| b.1.rep.iter().sum::<u64>().cmp(&a.1.rep.iter().sum::<u64>()));
    if rows.is_empty() {
        return;
    }
    // ⭐ONE walk of the interned-name pool resolves EVERY heap class word the table holds.
    // `Heap::class_of` takes a HANDLE, and handles die — the census key is the class WORD,
    // the integer `fu_kind_of` memoises and the emitted guards compare — and every object's
    // `Arc<str>` class was minted by `Heap::intern` out of this one pool, so the pool IS
    // the word's exact inverse. The arena half resolves by id through `nextgen::class_name`.
    let pool = vm.heap.class_pool.read().unwrap_or_else(|e| e.into_inner());
    let mut words: crate::FastMap<i64, &str> = crate::FastMap::default();
    for (name, arc) in pool.iter() {
        // The same first-word read `Heap::object_class_word` performs on the object.
        words.insert(unsafe { *(arc as *const std::sync::Arc<str> as *const i64) }, &**name);
    }
    for (&(rkey, _name, caller, cp), row) in rows.into_iter().take(40) {
        let n: u64 = row.rep.iter().sum();
        // SAFETY: the loader never unloads — the same dump-time discipline as
        // `choke_residue::dump` and `sitemono::dump_table`.
        let caller: &crate::class_loader::LoadedClass = unsafe { &*(caller as *const _) };
        let site = match caller.file.pool.member_ref(cp) {
            Some(m) => format!("{}.{}{}", m.owner, m.name, m.descriptor),
            None => format!("{}.?cp{cp}", caller.file.this_class),
        };
        let recv = if rkey == 0 {
            "<unclassified>".to_string()
        } else if rkey & 1 == 1 {
            crate::nextgen::class_name((rkey >> 1) as u32)
                .map_or_else(|| format!("?cid{}", rkey >> 1), |s| s.to_string())
        } else {
            words.get(&(rkey as i64)).map_or_else(|| format!("?word{rkey:#x}"), |s| s.to_string())
        };
        eprintln!(
            "[FU-MISS] n={n} recv={recv} rep={}/{}/{} at={} site={site}",
            row.rep[0],
            row.rep[1],
            row.rep[2],
            caller.file.this_class,
        );
    }
}
