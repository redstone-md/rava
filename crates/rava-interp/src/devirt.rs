//! DEVIRTUALISATION MONOMORPHISM PROBE (RAVA_DEVIRT_PROBE, read-only telemetry). At each
//! invokevirtual/invokeinterface call site (both route through `invokevirtual`), record the SET
//! of distinct receiver concrete classes seen. The aggregate answers: what fraction of hot virtual
//! call sites are monomorphic (1 class → devirtualisable to a direct call → LLVM can inline the
//! whole OOP graph), oligomorphic (2-3), or polymorphic/megamorphic. Default off → zero effect on
//! codegen or determinism.

use std::collections::HashMap;
use std::sync::Mutex;
/// site (caller-class ptr, bytecode index) → distinct receiver class-keys (capped at CAP; a
/// site that reaches CAP is treated as megamorphic — stop recording it).
static SITES: Mutex<Option<HashMap<(usize, u16), Vec<u64>>>> = Mutex::new(None);
const CAP: usize = 16;

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DEVIRT_PROBE").is_some())
}

/// Cheap stable per-class key for a receiver — no String alloc. Arena object → its class id
/// (tagged); heap ref → its interned class-name Arc pointer. Non-objects → None (skip).
pub(crate) fn recv_key(vm: &crate::Vm, recv: &crate::Value) -> Option<u64> {
    match recv {
        crate::Value::Ref(h) => vm.heap.class_of(*h).map(|c| {
            // FNV-1a of the class name — a stable per-class key, interning-independent.
            let mut hsh = 0xcbf2_9ce4_8422_2325u64;
            for b in c.bytes() {
                hsh ^= b as u64;
                hsh = hsh.wrapping_mul(0x0100_0000_01b3);
            }
            hsh
        }),
        #[cfg(feature = "next-gen-gc")]
        crate::Value::Obj(p) => Some((crate::nextgen::arena_class_id(*p) as u64) | (1u64 << 48)),
        _ => None,
    }
}

pub(crate) fn record(site: (usize, u16), key: u64) {
    let mut g = SITES.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    let v = m.entry(site).or_default();
    if v.len() < CAP && !v.contains(&key) {
        v.push(key);
    }
}

/// Distinct receiver class-count recorded (by the warmup probe) at a site, if any.
/// The LLVM devirt scaffold ([`crate::Vm::build_llvm_module`]) reads this to decide a
/// monomorphic (count == 1) invokevirtual/interface site. `None` = never recorded.
pub(crate) fn site_type_count(site: (usize, u16)) -> Option<usize> {
    let g = SITES.lock().ok()?;
    g.as_ref()?.get(&site).map(|v| v.len())
}

/// Recorded receiver class-keys at a site (clone — the caller holds no lock). For a
/// monomorphic site the single element is the class the LLVM guard would speculate on.
pub(crate) fn site_keys(site: (usize, u16)) -> Option<Vec<u64>> {
    let g = SITES.lock().ok()?;
    g.as_ref()?.get(&site).cloned()
}

// ---- STEADY-STATE probe (RAVA_DEVIRT_STEADY_PROBE) --------------------------------
// The warmup `SITES` probe samples the INTERPRETER's invokevirtual before the method
// compiles, biasing toward monomorphism. This one samples the COMPILED 4-way machine-
// code PIC's way-allocation path: `emit_dispatch_pic`'s miss callback (`rava_dispatch_miss`)
// fires exactly when a receiver class is not yet cached at that site — i.e. a WAY GETS
// ALLOCATED. Keyed by the per-site baked cache pointer (one per compiled dispatch site),
// it records the distinct receiver classes that miss there = the real steady-state
// polymorphism of arena virtual dispatch. Read-only, gated, off → zero effect.
/// cache_ptr → (site (caller_ptr, cp), distinct receiver class NAMES). Names captured directly
/// so the dump can resolve both the site and its receivers without a reverse key map.
static STEADY: Mutex<Option<HashMap<usize, ((usize, u16), Vec<String>)>>> = Mutex::new(None);

pub fn steady_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("RAVA_DEVIRT_STEADY_PROBE").is_some();
        if on {
            // A server boot never returns from guest main, so the exit-chain dump in run.rs is
            // unreachable. Spawn a periodic dumper (default 5s) so the census surfaces without a
            // clean shutdown. RAVA_DEVIRT_STEADY_EVERY_MS overrides.
            let every_ms = std::env::var("RAVA_DEVIRT_STEADY_EVERY_MS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|n| *n >= 500)
                .unwrap_or(5000);
            std::thread::Builder::new()
                .name("rava-devirt-steady".into())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(every_ms));
                    dump_steady();
                })
                .ok();
        }
        on
    })
}

/// Record a way-allocation (a miss at compiled site `cache_ptr` for `class_name`). Deduped,
/// capped — a site reaching CAP distinct classes is megamorphic (stop recording). `site` is the
/// (caller_ptr, cp) pair so the dump can resolve owner/name/descriptor.
pub(crate) fn record_steady(cache_ptr: usize, site: (usize, u16), class_name: &str) {
    let mut g = STEADY.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    let (_s, v) = m.entry(cache_ptr).or_insert_with(|| (site, Vec::new()));
    if v.len() < CAP && !v.iter().any(|c| c == class_name) {
        v.push(class_name.to_string());
    }
}

// 8.1b LLVM devirt-eligibility tally. `build_llvm_module` increments these as it discovers
// monomorphic 0xb6/0xb9 sites (lazy, aggregate — no per-module stderr). Dumped at exit.
static LLVM_ELIG_SITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LLVM_ELIG_MODULES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
// ARMABLE = of the eligible mono sites, those whose target method M is itself LLVM-emittable
// (numeric subset — no getfield/ref/nested-invoke). Only these can actually arm a devirt guard:
// the fast path IS `emit_llvm_fn(M)`, which bails on any object/field op.
static LLVM_ARMABLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn note_llvm_eligible(sites: usize, armable: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    LLVM_ELIG_SITES.fetch_add(sites as u64, Relaxed);
    let m = LLVM_ELIG_MODULES.fetch_add(1, Relaxed) + 1;
    LLVM_ARMABLE.fetch_add(armable as u64, Relaxed);
    // LLVM_AOT never completes a full MC boot (it attempts+bails per method) — emit a running
    // tally every 2000 module trees so a killed run still yields the eligible/armable ratio.
    if m % 2000 == 0 {
        dump_llvm_eligibility();
    }
}

// Memo for llvm_method_emittable: (owner#name#desc) FNV key -> armable. The same virtual
// targets recur across thousands of compiled callers; without this, each recompile re-walks
// the class hierarchy + rescans the callee (the 8.1c-probe boot-slowdown culprit).
static EMIT_MEMO: Mutex<Option<HashMap<u64, bool>>> = Mutex::new(None);

pub(crate) fn emit_memo_get(key: u64) -> Option<bool> {
    let g = EMIT_MEMO.lock().ok()?;
    g.as_ref()?.get(&key).copied()
}
pub(crate) fn emit_memo_put(key: u64, v: bool) {
    let mut g = EMIT_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new).insert(key, v);
}
/// (distinct target methods checked, of which LLVM-emittable) from the memo — the method-level
/// unlock count (the site tally can double-count a popular target).
pub(crate) fn emit_memo_stats() -> (usize, usize) {
    let g = EMIT_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        Some(m) => (m.len(), m.values().filter(|&&v| v).count()),
        None => (0, 0),
    }
}

// 8.2b cascade memo (separate from the flat/primitive EMIT_MEMO so the probes don't cross-
// contaminate) + the set of method names found cascade-emittable (for worldgen-core detection).
static CASCADE_MEMO: Mutex<Option<HashMap<u64, bool>>> = Mutex::new(None);
static EMITTABLE_NAMES: Mutex<Option<Vec<String>>> = Mutex::new(None);

pub(crate) fn cascade_memo_get(key: u64) -> Option<bool> {
    let g = CASCADE_MEMO.lock().ok()?;
    g.as_ref()?.get(&key).copied()
}
pub(crate) fn cascade_memo_put(key: u64, v: bool) {
    let mut g = CASCADE_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new).insert(key, v);
}
/// (distinct methods checked, of which cascade-emittable) — the full-cascade method-level unlock.
pub(crate) fn cascade_memo_stats() -> (usize, usize) {
    let g = CASCADE_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        Some(m) => (m.len(), m.values().filter(|&&v| v).count()),
        None => (0, 0),
    }
}
pub(crate) fn record_emittable(name: String) {
    let mut g = EMITTABLE_NAMES.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(Vec::new).push(name);
}

/// Worldgen-core reach: of the cascade-emittable methods, how many live in worldgen packages
/// (levelgen/densityfunction/biome/noise) and how many are named `compute` — the DensityFunction
/// hot core. Returns (worldgen_methods, compute_methods, a few sample names).
pub(crate) fn cascade_worldgen_reach() -> (usize, usize, Vec<String>) {
    let g = EMITTABLE_NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(v) = g.as_ref() else { return (0, 0, Vec::new()) };
    let is_wg = |s: &str| {
        let l = s.to_ascii_lowercase();
        l.contains("levelgen")
            || l.contains("densityfunction")
            || l.contains("biome")
            || l.contains("noise")
            || l.contains("synth")
    };
    let wg: Vec<&String> = v.iter().filter(|s| is_wg(s)).collect();
    let compute = v.iter().filter(|s| s.contains("#compute")).count();
    let sample: Vec<String> = wg.iter().take(8).map(|s| (*s).clone()).collect();
    (wg.len(), compute, sample)
}

pub(crate) fn emit_memo_key(owner: &str, name: &str, desc: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for s in [owner, "#", name, "#", desc] {
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// Print the LLVM devirt-eligibility tally (call at exit under either probe).
pub fn dump_llvm_eligibility() {
    use std::sync::atomic::Ordering::Relaxed;
    let (s, m) = (LLVM_ELIG_SITES.load(Relaxed), LLVM_ELIG_MODULES.load(Relaxed));
    if m > 0 {
        let armable = LLVM_ARMABLE.load(Relaxed);
        #[cfg(feature = "next-gen-gc")]
        let cascade = crate::llvm_cascade_probe();
        #[cfg(not(feature = "next-gen-gc"))]
        let cascade = false;
        let (methods, emittable) =
            if cascade { cascade_memo_stats() } else { emit_memo_stats() };
        eprintln!(
            "[DEVIRT-LLVM] {s} monomorphic virtual site(s) LLVM_Devirt_Eligible across {m} module tree(s); {armable} ARMABLE site(s); target methods: {emittable}/{methods} LLVM-emittable"
        );
        #[cfg(feature = "next-gen-gc")]
        if cascade {
            let (wg, compute, sample) = cascade_worldgen_reach();
            eprintln!(
                "[DEVIRT-CASCADE] worldgen-core reach: {wg} emittable method(s) in worldgen packages, {compute} named `compute`. samples: {sample:?}"
            );
        }
    }
}

/// Aggregate + print the steady-state (compiled-PIC) monomorphism chart (call at exit),
/// plus the top-20 most polymorphic sites with their caller@cp and distinct receiver classes.
pub fn dump_steady() {
    let g = STEADY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = g.as_ref() else {
        eprintln!("[DEVIRT-STEADY] no compiled dispatch sites recorded");
        return;
    };
    let (mut mono, mut oligo, mut poly, mut mega) = (0u64, 0u64, 0u64, 0u64);
    for ((_, classes)) in m.values() {
        let n = classes.len();
        match n {
            0 => {}
            1 => mono += 1,
            2..=3 => oligo += 1,
            4..=15 => poly += 1,
            _ => mega += 1,
        };
    }
    let total = mono + oligo + poly + mega;
    let pct = |x: u64| if total > 0 { x * 100 / total } else { 0 };
    eprintln!(
        "[DEVIRT-STEADY] {total} compiled dispatch sites (way-alloc): \
         MONOMORPHIC={mono} ({}%) oligomorphic(2-3)={oligo} ({}%) \
         polymorphic(4-15)={poly} ({}%) megamorphic(16+)={mega} ({}%)",
        pct(mono), pct(oligo), pct(poly), pct(mega)
    );
    // Top sites: sort by distinct-receiver count, show histogram per site.
    let mut rows: Vec<((usize, u16), &[String])> = m.values().map(|(site, v)| (*site, v.as_slice())).collect();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    eprintln!("[DEVIRT-STEADY] top-20 polymorphic sites (distinct receiver classes):");
    for ((caller, cp), classes) in rows.into_iter().take(20) {
        let mref: Option<rava_classfile::MemberRef> = if caller != 0 && cp as u32 as usize != 0 {
            // SAFETY: LoadedClass is never freed (loader pin), dump-time only.
            let lc: &crate::class_loader::LoadedClass = unsafe { &*(caller as *const _) };
            lc.file.pool.member_ref(cp)
        } else { None };
        let site_name = mref.as_ref().map(|r| format!("{}.{}{}", r.owner, r.name, r.descriptor))
            .unwrap_or_else(|| format!("@cp{cp}"));
        let caller_name = if caller != 0 {
            let lc: &crate::class_loader::LoadedClass = unsafe { &*(caller as *const _) };
            lc.file.this_class.to_string()
        } else { "<no-caller>".to_string() };
        eprintln!("[DEVIRT-STEADY]   {} classes @ {} -> {}  receivers={:?}", classes.len(), caller_name, site_name, classes);
    }
}

/// Aggregate + print the monomorphism chart (call at program exit under the gate).
pub fn dump() {
    let g = SITES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = g.as_ref() else {
        eprintln!("[DEVIRT-PROBE] no virtual call sites recorded");
        return;
    };
    let (mut mono, mut oligo, mut poly, mut mega) = (0u64, 0u64, 0u64, 0u64);
    for v in m.values() {
        match v.len() {
            0 => {}
            1 => mono += 1,
            2..=3 => oligo += 1,
            4..=15 => poly += 1,
            _ => mega += 1, // hit CAP
        }
    }
    let total = mono + oligo + poly + mega;
    let pct = |x: u64| if total > 0 { x * 100 / total } else { 0 };
    eprintln!(
        "[DEVIRT-PROBE] {total} hot virtual call sites: \
         MONOMORPHIC={mono} ({}%) oligomorphic(2-3)={oligo} ({}%) \
         polymorphic(4-15)={poly} ({}%) megamorphic(16+)={mega} ({}%)",
        pct(mono), pct(oligo), pct(poly), pct(mega)
    );
}
