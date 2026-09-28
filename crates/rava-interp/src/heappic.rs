//! 23.4a HEAP-REF PIC SHADOW (RAVA_JIT_HEAPPIC_SHADOW, inert). The combat step (23.4) extends the
//! machine-code dispatch PIC to heap-Ref (non-arena) receivers — the 60-67% of misses. Before any live
//! call_indirect can act on a machine-code-read class key, this shadow PROVES the read is bit-correct:
//! emit_dispatch_pic (for DiscreteVoxelShape sites) reads the receiver's class key inline in machine
//! code (objects_base + handle*8 → box_ptr → +offset_of(class) → Arc word) and passes it to
//! `rava_heap_pic_shadow`, which compares it against the authoritative Rust `Heap::object_class_word`.
//! It NEVER changes dispatch (normal is_arena→pic→miss flow is untouched) ⇒ bit-exact by construction;
//! a wrong read would surface as a crash in the gate run (→ revert), never a silent divergence.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

pub fn shadow_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_HEAPPIC_SHADOW").is_some())
}

/// 23.4c LIVE heap-Ref PIC — DEFAULT-ON (kill-switch RAVA_NO_JIT_HEAPPIC=1). Every virtual/interface
/// site emits a machine-code heap-Ref 4-way compare → call_indirect on HIT (bypassing rava_dispatch_
/// miss); rava_dispatch_miss fills (arc_key, GC-leaf fptr) into the site cache. Verified bit-exact
/// (RingScan 109205b3 / ChunkFill 8790 / Decode a17dbfa0) + ngg 228 / default 108; cuts ~15M
/// dispatch-misses per bench. Leaf-only ⇒ no rooting on the fast path.
pub fn live_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_JIT_HEAPPIC").is_none())
}

static MATCH: AtomicU64 = AtomicU64::new(0);
static MISMATCH: AtomicU64 = AtomicU64::new(0);
/// 23.4b live-PIC accounting: (site,class) cache fills + never-cacheable negatives.
pub static FILL: AtomicU64 = AtomicU64::new(0);
pub static FILL_NEG: AtomicU64 = AtomicU64::new(0);
/// `NotYet` verdicts — the callee exists but is not compiled, so the fill writes NOTHING and the
/// very next call re-runs the whole resolve (two `String`s plus a `resolve_method` walk).
///
/// ⭐**The ARENA path has had a memo for this since row 129 (`notyet_memo`/`RAVA_NO_NOTYET_MEMO`,
/// priced at 4,349,170 re-resolves on one bench); the heap-Ref path never got one — and the handle
/// heap is where the collections live.** This counter is what says whether that asymmetry costs
/// anything before a second memo is written to close it.
pub static FILL_NOTYET: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn note_fill(negative: bool) {
    if crate::picprobe::enabled() {
        (if negative { &FILL_NEG } else { &FILL }).fetch_add(1, Relaxed);
    }
}

/// emit-time dispatch mode for a call site: 0=none, 1=shadow-verify, 2=live PIC.
pub fn emit_mode() -> u8 {
    if live_enabled() {
        2
    } else if shadow_enabled() {
        1
    } else {
        0
    }
}

/// Verify the machine-code-extracted class key against the authoritative Rust word. Read-only.
pub fn shadow_check(heap: &crate::heap::Heap, handle: u32, machine_key: i64) {
    let rust_key = heap.object_class_word(handle).unwrap_or(0);
    if machine_key != 0 && machine_key == rust_key {
        MATCH.fetch_add(1, Relaxed);
    } else {
        let n = MISMATCH.fetch_add(1, Relaxed) + 1;
        if n <= 20 {
            eprintln!(
                "[HEAPPIC-SHADOW-MISMATCH] handle={handle} machine_key={machine_key:#x} rust_key={rust_key:#x}"
            );
        }
    }
}

pub fn dump() {
    if shadow_enabled() {
        let (m, mm) = (MATCH.load(Relaxed), MISMATCH.load(Relaxed));
        let t = m + mm;
        let pct = if t > 0 { m * 10000 / t } else { 0 };
        eprintln!(
            "[HEAPPIC-SHADOW] DiscreteVoxelShape heap-Ref class-key reads: match={m} mismatch={mm} total={t} ({}.{:02}% match)",
            pct / 100,
            pct % 100
        );
        if mm == 0 && m > 0 {
            eprintln!("[HEAPPIC-SHADOW] ⇒ machine-code heap-Ref class read is 100% correct — safe to arm the live PIC (23.4b).");
        }
    }
    // Default-on ⇒ keep prod boots quiet; only report the live tally under a diagnostic gate.
    if live_enabled() && crate::picprobe::enabled() {
        eprintln!(
            "[HEAPPIC-LIVE] leaf heap-Ref PIC (default-on): cache fills={} never-cacheable={} (each fill converts a (site,class) to machine-code hits)",
            FILL.load(Relaxed),
            FILL_NEG.load(Relaxed)
        );
        eprintln!(
            "[HEAPPIC-LIVE] NotYet verdicts (fill writes nothing => next call re-resolves): {}",
            FILL_NOTYET.load(Relaxed)
        );
    }
}
