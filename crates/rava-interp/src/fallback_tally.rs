//! COMPILED→INTERPRETER FALLBACK TALLY (`RAVA_FALLBACK_TALLY=1`, default OFF,
//! diagnostic only — counts, no walls, contention-immune).
//!
//! The RIP profile shows interpreter opcode handlers (`opcodes::getstatic`,
//! `opcodes::invokevirtual`, …) reached FROM compiled code: a compiled body hits
//! an operation its codegen cannot serve inline and falls back into the host.
//! This tally answers WHICH sites fall out and (at dump time) WHY, keyed by
//! `(fallback kind, caller class, cp index)` — a two-word hash + one counter
//! increment on the armed path, ZERO work when the flag is unset (one lazy bool).
//!
//! No String is built at note time (the O(string)-per-call bookkeeping defect
//! class); symbols are resolved once, at dump.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// What was counted. Invoke variants preserve the real invoke opcode;
/// direct-mono variants isolate permanent-negative and cold-cache callback
/// entries. Getstatic splits its two fallback shapes.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// General invoke fallback: `jit_call` found no compiled direct callee, so the
    /// call runs through the interpreter's `opcodes::invoke*` gauntlet.
    /// Payload = the invoke opcode (0xb6/0xb7/0xb8/0xb9).
    Invoke(u8),
    /// Permanent-negative direct-mono callback entry, distinct from downstream
    /// invoke fallbacks reached through `jit_call_body`.
    DirectMono(u8),
    /// Cold zero-word direct-mono callback entry, before the site's cache verdict.
    DirectMonoCold(u8),
    /// getstatic with NO value cell (a reference-typed static today): every
    /// execution takes the full `opcodes::getstatic` callback.
    GetstaticUncached,
    /// getstatic value-cell MISS (cold cell or a `static_gen` bump): the refill
    /// callback. A hot count here = the gen is being bumped between reads.
    GetstaticMiss,
    /// `instanceof` (0xc1). The codegen emits NO inline test at all — every
    /// execution is a full callback into `opcodes::instance_of`. Counted so the
    /// type-test fallback ranks in the SAME table as the invoke rows instead of
    /// being argued about from the RIP profile alone.
    Instanceof,
}

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FALLBACK_TALLY").is_some())
}

/// `(kind, caller LoadedClass ptr, cp)` → count. The class ptr is stable for the
/// process lifetime (the loader never unloads), so it is a safe dump-time key.
static TALLY: Mutex<Option<HashMap<(Kind, usize, u16), u64>>> = Mutex::new(None);

#[inline]
pub(crate) fn note(kind: Kind, caller: usize, cp: u16) {
    if !enabled() {
        return;
    }
    if let Ok(mut t) = TALLY.lock() {
        *t.get_or_insert_with(HashMap::new)
            .entry((kind, caller, cp))
            .or_insert(0) += 1;
    }
}

/// Map the `jit_call` kind ordinal (0=static 1=virtual 2=special 3=interface) to
/// the real invoke opcode, so the dump reads in JVMS terms.
#[inline]
pub(crate) fn invoke_kind_op(kind: u8) -> u8 {
    match kind {
        0 => 0xb8,
        2 => 0xb7,
        3 => 0xb9,
        _ => 0xb6,
    }
}

fn invoke_label(op: u8) -> &'static str {
    match op {
        0xb8 => "invokestatic",
        0xb7 => "invokespecial",
        0xb9 => "invokeinterface",
        _ => "invokevirtual",
    }
}

fn direct_mono_label(op: u8) -> &'static str {
    match op {
        0xb8 => "direct-mono-invokestatic",
        0xb7 => "direct-mono-invokespecial",
        _ => "direct-mono-invoke",
    }
}

fn direct_mono_cold_label(op: u8) -> &'static str {
    match op {
        0xb8 => "direct-mono-cold-invokestatic",
        0xb7 => "direct-mono-cold-invokespecial",
        _ => "direct-mono-cold-invoke",
    }
}

fn invoke_why_at(
    vm: &crate::Vm,
    caller: &crate::class_loader::LoadedClass,
    cp: u16,
    op: u8,
) -> String {
    if matches!(op, 0xb7 | 0xb8) {
        caller
            .file
            .pool
            .member_ref(cp)
            .map(|m| invoke_why(vm, m.owner, m.name, m.descriptor))
            .unwrap_or_default()
    } else {
        " why=receiver-dependent".to_string()
    }
}

/// One-line-per-site dump, top 40 by count. `vm` names the WHY for statically
/// resolvable callees (invokestatic/special): native-served, no bytecode, or the
/// callee's `jit_cache` verdict (Failed = refused compile, Cold = below
/// threshold, Compiled = the DIRECT-CALL cache missed, not the compiler).
pub(crate) fn dump(vm: &crate::Vm) {
    if !enabled() {
        return;
    }
    let Ok(t) = TALLY.lock() else { return };
    let Some(t) = t.as_ref() else { return };
    let mut rows: Vec<(&(Kind, usize, u16), &u64)> = t.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    let total: u64 = t.values().sum();
    eprintln!("[FB-TALLY] {} sites, {} fallbacks total", rows.len(), total);
    instanceof_memo_census();
    for (&(kind, caller, cp), &n) in rows.into_iter().take(40) {
        let caller: &crate::class_loader::LoadedClass =
            unsafe { &*(caller as *const crate::class_loader::LoadedClass) };
        // `instanceof`'s cp entry is a CLASS, every other kind's is a member ref.
        let sym = if kind == Kind::Instanceof {
            caller.file.pool.class_name(cp).map_or_else(
                || format!("{}.?cp{cp}", caller.file.this_class),
                |t| format!("{} instanceof {t}", caller.file.this_class),
            )
        } else {
            caller.file.pool.member_ref(cp).map_or_else(
                || format!("{}.?cp{cp}", caller.file.this_class),
                |m| format!("{}.{}{}", m.owner, m.name, m.descriptor),
            )
        };
        let (kname, why) = match kind {
            Kind::GetstaticUncached => ("getstatic-uncached", String::new()),
            Kind::GetstaticMiss => ("getstatic-cell-miss", String::new()),
            Kind::Instanceof => ("instanceof", " why=no-inline-type-test".to_string()),
            Kind::Invoke(op) => (invoke_label(op), invoke_why_at(vm, caller, cp, op)),
            Kind::DirectMono(op) => (direct_mono_label(op), invoke_why_at(vm, caller, cp, op)),
            Kind::DirectMonoCold(op) => (
                direct_mono_cold_label(op),
                invoke_why_at(vm, caller, cp, op),
            ),
        };
        eprintln!("[FB-TALLY] {kname} n={n} sym={sym}{why}");
    }
    // `DirectMono` counts negative-cache arrivals and `DirectMonoCold` the
    // initial zero-word arrival. `Invoke` is noted only after the inner compiled
    // direct-cache/AOT path declines, before structural/native fastpaths.
    // On a serial boot their difference estimates calls served before fallback;
    // excess `Invoke` shows other ingress.
    let mut direct_routes: Vec<(u8, usize, u16, u64)> = t
        .iter()
        .filter_map(|(&(kind, caller, cp), &direct)| match kind {
            Kind::DirectMono(op) => Some((op, caller, cp, direct)),
            _ => None,
        })
        .collect();
    direct_routes.sort_by(|a, b| b.3.cmp(&a.3));
    for (op, caller_ptr, cp, negative) in direct_routes.into_iter().take(20) {
        let cold = t
            .get(&(Kind::DirectMonoCold(op), caller_ptr, cp))
            .copied()
            .unwrap_or(0);
        let direct = negative + cold;
        let fallback_entry = t
            .get(&(Kind::Invoke(op), caller_ptr, cp))
            .copied()
            .unwrap_or(0);
        let status = if fallback_entry <= direct {
            "paired"
        } else {
            "counter-mismatch"
        };
        let pre_fallback = direct.saturating_sub(fallback_entry);
        let caller: &crate::class_loader::LoadedClass =
            unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
        let sym = caller.file.pool.member_ref(cp).map_or_else(
            || format!("{}.?cp{cp}", caller.file.this_class),
            |m| format!("{}.{}{}", m.owner, m.name, m.descriptor),
        );
        eprintln!(
            "[DM-ROUTE] site={}.cp{cp} target={sym} negative={negative} cold={cold} fallback_entry={fallback_entry} pre_fallback={pre_fallback} status={status}",
            caller.file.this_class
        );
    }
}

/// `RAVA_INSTANCEOF_MEMO` hit/miss/unkeyed, so the type-test memo is REFUTABLE
/// rather than assumed: `hit` = walks removed, `miss` = first (site, class) pair,
/// `unkeyed` = receivers deliberately excluded from the memo (lambdas, whose
/// answer is a per-OBJECT property, and the shapes `instance_of` already answers
/// in O(1)). Silent when the memo is off (all three zero).
fn instanceof_memo_census() {
    use std::sync::atomic::Ordering::Relaxed;
    let (h, m, u) = (
        crate::opcodes::IOF_HIT.load(Relaxed),
        crate::opcodes::IOF_MISS.load(Relaxed),
        crate::opcodes::IOF_UNKEYED.load(Relaxed),
    );
    if h | m | u == 0 {
        return;
    }
    let served = h as f64 * 100.0 / (h + m + u).max(1) as f64;
    eprintln!("[FB-TALLY] instanceof-memo hit={h} miss={m} unkeyed={u} served={served:.1}%");
}

/// Best-effort WHY for a statically named callee, using only already-loaded
/// classes (never triggers a load from a diagnostic).
fn invoke_why(vm: &crate::Vm, owner: &str, name: &str, desc: &str) -> String {
    let Some(cls) = vm.loader.cached(owner) else {
        // Not in the loader cache — for JDK owners this usually means the class is
        // NATIVE-SERVED (java/lang/Long etc.), so every call runs the host serve.
        return " why=owner-unloaded(native-served?)".to_string();
    };
    let Some(m) = cls.file.method(name, desc) else {
        return " why=not-declared-here(super/native?)".to_string();
    };
    let Some(code) = m.code.as_ref() else {
        return " why=no-bytecode(native)".to_string();
    };
    let key = code as *const rava_classfile::Code as usize;
    match vm.jit_cache.get(&key) {
        Some(crate::jit::JitStatus::Compiled(_)) => " why=compiled(direct-miss)".to_string(),
        Some(crate::jit::JitStatus::Compiling) => " why=background-compiling".to_string(),
        Some(crate::jit::JitStatus::Failed) => " why=compile-refused".to_string(),
        Some(crate::jit::JitStatus::Cold(n)) => format!(" why=cold(n={n})"),
        None => " why=never-counted".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_mono_site_labels_are_distinct() {
        assert_eq!(direct_mono_label(0xb8), "direct-mono-invokestatic");
        assert_eq!(direct_mono_label(0xb7), "direct-mono-invokespecial");
        assert_eq!(
            direct_mono_cold_label(0xb8),
            "direct-mono-cold-invokestatic"
        );
        assert_eq!(
            direct_mono_cold_label(0xb7),
            "direct-mono-cold-invokespecial"
        );
        assert_eq!(invoke_label(0xb8), "invokestatic");
        assert_eq!(invoke_label(0xb7), "invokespecial");
        assert!(Kind::DirectMono(0xb7) != Kind::Invoke(0xb7));
        assert!(Kind::DirectMono(0xb8) != Kind::DirectMonoCold(0xb8));
    }
}
