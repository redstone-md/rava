//! CHOKE-RESIDUE CENSUS (`RAVA_CHOKE_RESIDUE=1`, default OFF; `RAVA_NO_CHOKE_RESIDUE=1`
//! withdraws, `RAVA_NO_EMIT_MORE=1` blocks) — observe-only, never routes.
//!
//! The whole-prologue meter counts ~21.0 M compiled→host crossings a boot; the epilogue,
//! reached only by the general path, counts ~8.4 M. **12.6 M crossings are served by the
//! early arms (direct call, ring, getter, SAM, iterator, worker-exec) — this census counts
//! the residue the WHOLE ladder refuses: the crossings that fall all the way through and
//! build a synthetic frame.** It notes immediately before `seam_prologue_full` starts —
//! every early return is upstream of that line — so by construction the census total
//! equals the meter's event count in the same run; the dump header prints both and a
//! MATCH/MISMATCH verdict.
//!
//! Same discipline as `fallback_tally`: keyed `(kind, caller class ptr, cp)`, no String
//! at note time (the O(string)-per-call defect class), symbols and the WHY — native /
//! body shape / jit verdict — resolved once at dump. The receiver's representation is
//! classified from the raw cell's TAG BITS ONLY; nothing is rooted, cloned or unboxed at
//! note time. `fallback_tally::note` counts ENTRIES into the fallback path; the difference
//! between its table and this one is exactly what the arms serve.

use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{LazyLock, Mutex};

/// Kill switch (`RAVA_NO_CHOKE_RESIDUE=1`), then the block switch, then the arming flag —
/// so `RAVA_NO_EMIT_MORE` bisects this census out together with the rest of the block in
/// one run.
static ON: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("RAVA_NO_CHOKE_RESIDUE").is_none()
        && !crate::flags::emit_more_disabled()
        && std::env::var_os("RAVA_CHOKE_RESIDUE").is_some()
});

pub(crate) fn enabled() -> bool {
    *ON
}

/// `(kind, caller LoadedClass ptr, cp)` → counts. The class ptr is stable for the process
/// lifetime (the loader never unloads), so it is a safe dump-time key — `fallback_tally`'s
/// key, with a receiver-representation split instead of one bare number.
static TALLY: Mutex<Option<HashMap<(u8, usize, u16), Row>>> = Mutex::new(None);

/// One site's counts. `argc` is the raw cell count per crossing (receiver included for
/// instance calls) — a constant of the call's ABI. `rep` splits the receiver's arrival
/// representation from the cell's tag bits: heap-Ref / arena-Obj / other-or-null /
/// static-na.
struct Row {
    argc: u8,
    rep: [u64; 4],
}

#[inline]
pub(crate) fn note(kind: u8, caller: usize, cp: u16, raw: &[i64]) {
    if !enabled() {
        return;
    }
    // TAG BITS ONLY — the `refpot`/`unbox_ref` idioms; never unbox, root or clone here.
    let bucket = if kind == 0 {
        3 // invokestatic: no receiver cell
    } else {
        let r = raw.first().copied().unwrap_or(0) as u64;
        if r != 0 && r >> 32 == (crate::value::REF_TAG as u64) >> 32 {
            0 // heap Ref (REF_TAG 1<<40)
        } else if r != 0 && r >> 40 == 0 {
            1 // bare arena pointer, pinned mirrors included: low address, no high-word tag
        } else {
            2 // null / box / array / anything else
        }
    };
    if let Ok(mut t) = TALLY.lock() {
        t.get_or_insert_with(HashMap::new)
            .entry((kind, caller, cp))
            .or_insert_with(|| Row { argc: raw.len().min(u8::MAX as usize) as u8, rep: [0; 4] })
            .rep[bucket] += 1;
    }
}

/// Top 40 sites by count, one line each, plus the MATCH self-check against
/// `seam_prologue_full`. The meter's cell layout is `[events, sampled, cycles]`
/// (`C_EVENTS`/`C_SAMPLED`/`C_CYCLES`; `diag::meter` is a private module, so the indices
/// are literals here).
pub(crate) fn dump(vm: &crate::Vm) {
    if !enabled() {
        return;
    }
    // The meter cells and the calibration are read BEFORE the TALLY lock: the cells are
    // atomics, and `per_ns` spins ~2 ms — no reason to hold the note path's lock for it.
    let meter = crate::diag::seam_prologue_full();
    let (ev, sampled, cycles) = (
        meter.cells[0].load(Relaxed),
        meter.cells[1].load(Relaxed),
        meter.cells[2].load(Relaxed),
    );
    let mean_ns = if meter.armed && sampled > 0 {
        (cycles as f64 / sampled as f64) / per_ns()
    } else {
        0.0
    };
    let Ok(mut g) = TALLY.lock() else { return };
    // An empty map is a FINDING — "every crossing was served before the frame build" —
    // not a silent nothing. Always print the header; rows follow only if there are any.
    let map = g.get_or_insert_with(HashMap::new);
    let total: u64 = map.values().map(|r| r.rep.iter().sum::<u64>()).sum();
    let verdict = if !meter.armed {
        "meter-off(arm RAVA_DIAG=price for the cross-check)"
    } else if ev == total {
        "MATCH"
    } else {
        "MISMATCH!"
    };
    eprintln!("[CHOKE] residue={total} sites={} seam_prologue_full_ev={ev} {verdict}", map.len());
    let mut rows: Vec<(&(u8, usize, u16), &Row)> = map.iter().collect();
    rows.sort_by(|a, b| b.1.rep.iter().sum::<u64>().cmp(&a.1.rep.iter().sum::<u64>()));
    for (&(kind, caller, cp), row) in rows.into_iter().take(40) {
        let caller: &crate::class_loader::LoadedClass =
            unsafe { &*(caller as *const crate::class_loader::LoadedClass) };
        let n: u64 = row.rep.iter().sum();
        let (sym, why) = match caller.file.pool.member_ref(cp) {
            Some(m) => (
                format!("{}.{}{}", m.owner, m.name, m.descriptor),
                why(vm, kind, m.owner, m.name, m.descriptor),
            ),
            None => (format!("{}.?cp{cp}", caller.file.this_class), String::new()),
        };
        let est = if mean_ns > 0.0 {
            format!(" est={:.0}ns", n as f64 * mean_ns)
        } else {
            String::new()
        };
        eprintln!(
            "[CHOKE] {} n={n} argc={} rep={}/{}/{}/{} at={} sym={sym} why={why}{est}",
            kind_name(kind),
            row.argc,
            row.rep[0],
            row.rep[1],
            row.rep[2],
            row.rep[3],
            caller.file.this_class,
        );
    }
}

/// The `jit_call` kind ordinal in JVMS terms (0=static 1=virtual 2=special 3=interface).
fn kind_name(kind: u8) -> &'static str {
    match crate::fallback_tally::invoke_kind_op(kind) {
        0xb8 => "invokestatic",
        0xb7 => "invokespecial",
        0xb9 => "invokeinterface",
        _ => "invokevirtual",
    }
}

/// Best-effort WHY this crossing builds a frame, using only already-loaded classes
/// (never triggers a load from a diagnostic) — `fallback_tally::invoke_why`'s shape,
/// extended with a body-shape classifier: ring/getter/SAM are shapes a fastpath arm
/// recognises, so a residue row carrying one is a question about the ARM, not the body.
/// Virtual/interface rows classify the DECLARED owner's body and say so — the dispatched
/// target is receiver-dependent.
fn why(vm: &crate::Vm, kind: u8, owner: &str, name: &str, desc: &str) -> String {
    // Virtual/interface rows classify the DECLARED owner's body: the dispatched target is
    // receiver-dependent, so every verdict below is prefixed to say so. Static/special
    // resolve exactly, like `fallback_tally::invoke_why`.
    let declared = if kind == 1 || kind == 3 { "declared " } else { "" };
    let Some(cls) = vm.loader.cached(owner) else {
        return format!("{declared}owner-unloaded(native-served?)");
    };
    let Some(m) = cls.file.method(name, desc) else {
        return format!("{declared}not-declared-here(super/native?)");
    };
    let Some(code) = m.code.as_ref() else {
        return format!("{declared}no-code(native)");
    };
    let shape = if ring_shape(&code.bytecode) {
        "ring25"
    } else if crate::jit::trivial_getter::trivial_getfield(&code.bytecode).is_some() {
        "getter5"
    } else if sam_shape(m, code) {
        "sam<=16"
    } else {
        "none"
    };
    let jit = match vm.jit_cache.get(&(code as *const rava_classfile::Code as usize)) {
        Some(crate::jit::JitStatus::Compiled(_)) => "jit=compiled(direct-miss)".to_string(),
        Some(crate::jit::JitStatus::Compiling) => "jit=background-compiling".to_string(),
        Some(crate::jit::JitStatus::Failed) => "jit=compile-refused".to_string(),
        Some(crate::jit::JitStatus::Cold(n)) => format!("jit=cold(n={n})"),
        None => "jit=never-counted".to_string(),
    };
    format!("{declared}{shape}({}B) {jit}", code.bytecode.len())
}

/// The ring `size()` body, byte-for-byte — `fu_fastpath::ring_size_body_fields`'s compare
/// with the three field-index operand pairs wildcarded (a diagnostic recognises the SHAPE,
/// not the pool ids).
fn ring_shape(bc: &[u8]) -> bool {
    bc.len() == 25
        && bc[0] == 0x2a
        && bc[1] == 0xb4
        && bc[4] == 0x2a
        && bc[5] == 0xb4
        && bc[8] == 0x64
        && bc[9] == 0x3c
        && bc[10] == 0x1b
        && bc[11] == 0x9b
        && bc[12] == 0x00
        && bc[13] == 0x07
        && bc[14] == 0x1b
        && bc[15] == 0xa7
        && bc[16] == 0x00
        && bc[17] == 0x09
        && bc[18] == 0x2a
        && bc[19] == 0xb4
        && bc[22] == 0x1b
        && bc[23] == 0x60
        && bc[24] == 0xac
}

/// Whitelist approximation of `pure_sam_fastpath`'s admission (whose `verify_cfg` is
/// private): no handlers, not synchronized, ≤ 16 bytes, whitelist opcodes only. The real
/// arm additionally proves the mini-CFG; a "sam<=16" row here says SHAPE, not admissibility.
fn sam_shape(m: &rava_classfile::MemberInfo, code: &rava_classfile::Code) -> bool {
    // ACC_STATIC 0x0008, ACC_SYNCHRONIZED 0x0020 — the real arm's flag pair; no guest
    // name is consulted.
    m.access_flags & 0x0008 == 0x0008
        && m.access_flags & 0x0020 == 0
        && code.exceptions.is_empty()
        && !code.bytecode.is_empty()
        && code.bytecode.len() <= 16
        && code
            .bytecode
            .iter()
            .all(|&op| matches!(op, 0x02..=0x08 | 0x15 | 0x1a..=0x1d | 0x99 | 0x9a | 0x9f..=0xa4 | 0xa7 | 0xac))
}

/// rdtsc cycles per nanosecond, measured at dump time because `diag::meter`'s own
/// `calibration` is private — same technique and same 2 ms window, so the est-ns column
/// is comparable with the meter's own rows.
fn per_ns() -> f64 {
    // Off x86_64 `diag::meter`'s `now_cycles` counts nanoseconds, so cycles ARE ns.
    #[cfg(not(target_arch = "x86_64"))]
    return 1.0;
    static C: LazyLock<f64> = LazyLock::new(|| {
        let t0 = std::time::Instant::now();
        // SAFETY: `_rdtsc` is unconditionally available on x86_64 and only reads a counter.
        let c0 = unsafe { core::arch::x86_64::_rdtsc() };
        while t0.elapsed().as_micros() < 2000 {
            std::hint::spin_loop();
        }
        let cy = unsafe { core::arch::x86_64::_rdtsc() }.saturating_sub(c0);
        (cy as f64 / t0.elapsed().as_nanos().max(1) as f64).max(0.001)
    });
    *C
}
