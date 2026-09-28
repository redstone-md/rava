//! 23.3 HEAP-REF PIC POTENTIAL PROBE (RAVA_JIT_REF_POTENTIAL, inert, no guest alloc, no GC ref touch).
//! 23.2 found the machine-code PIC misses 95% of calls, 60-67% because the receiver is a NON-ARENA
//! heap Ref (emit_dispatch_pic's emit_is_arena gate rejects it). This probe measures the exact
//! REDUCIBLE slice: of those heap-Ref virtual/interface calls, how many resolve to a COMPILED fptr
//! (jit_direct_call HIT — the clean win a heap-Ref PIC in 23.4 would move into machine code) vs fall
//! to the native/java-served or tier-lag floor (jit_direct_call miss). Classified once per call at
//! jit_direct_call's return (no negative-cache sampling bias), grouped by call-site owner (the
//! DFU/Codec + JDK interfaces that dominate). Headline counters are lock-free atomics; the by-owner
//! grouping uses a gated Rust-side map (like [[icmiss]]) — no guest-arena allocation, no GC effect.
//!
//! 23.4 PREP — the heap-Ref class key a machine-code PIC will compare: `Heap::class_name_ptr(handle)`
//! = `objects[handle].class.as_ptr()`, the interned per-class Arc<str> data pointer (stable, shared by
//! all instances of a class — already the guard jit_direct_resolve uses for Ref receivers). A machcode
//! heap-Ref PIC reads it as: objects_vec_base + handle*sizeof(HeapObject) + offset_of(class) → load the
//! Arc data ptr, then the same 4-way compare the arena PIC does on the header class_id. The two
//! prerequisites to bake in 23.4: a stable base accessor for the objects table + the HeapObject layout.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_REF_POTENTIAL").is_some())
}

pub static NA_REDUCIBLE: AtomicU64 = AtomicU64::new(0); // heap-Ref virt/iface call → compiled fptr (clean win)
pub static NA_FLOOR: AtomicU64 = AtomicU64::new(0); // heap-Ref virt/iface call → no fptr (native/tier floor)
// owner (call-site static interface/class) -> [reducible, floor]. Bounded (~hundreds of owners).
const RED: usize = 0;
const FLR: usize = 1;
static BY_OWNER: Mutex<Option<HashMap<String, [u64; 2]>>> = Mutex::new(None);

/// `caller` + `cp` name the call-site's static owner (the interface, e.g. Function/DensityFunction).
/// `reducible` = jit_direct_call would serve it directly (compiled fptr). Read-only on the VM.
pub fn record(caller: &crate::class_loader::LoadedClass, cp: u16, reducible: bool) {
    if reducible {
        NA_REDUCIBLE.fetch_add(1, Relaxed);
    } else {
        NA_FLOOR.fetch_add(1, Relaxed);
    }
    let owner = match caller.file.pool.member_ref(cp) {
        Some(m) => m.owner,
        None => return,
    };
    let idx = if reducible { RED } else { FLR };
    let mut g = BY_OWNER.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    // Allocate a key String only the first time an owner is seen (bounded), then bump in place.
    if let Some(e) = m.get_mut(owner) {
        e[idx] += 1;
    } else {
        let mut e = [0u64; 2];
        e[idx] = 1;
        m.insert(owner.to_string(), e);
    }
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let (red, floor) = (NA_REDUCIBLE.load(Relaxed), NA_FLOOR.load(Relaxed));
    let total = red + floor;
    let pct = |x: u64| if total > 0 { format!("{}.{}%", x * 1000 / total / 10, x * 1000 / total % 10) } else { "0%".into() };
    eprintln!("[REF-POT] heap-Ref virtual/interface calls (the machcode-PIC-rejected non-arena population) = {total}");
    eprintln!("[REF-POT]   REDUCIBLE (resolves to compiled fptr — heap-Ref PIC win)  {red:>12} ({})", pct(red));
    eprintln!("[REF-POT]   FLOOR (native/java-served or not-compiled — irreducible)   {floor:>12} ({})", pct(floor));
    eprintln!("[REF-POT]   ⇒ extending the PIC to heap-Ref receivers moves {} of all heap-Ref virt/iface calls into machine code", pct(red));
    let g = BY_OWNER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = g.as_ref() {
        let mut rows: Vec<(&String, &[u64; 2])> = m.iter().collect();
        rows.sort_by(|a, b| (b.1[RED] + b.1[FLR]).cmp(&(a.1[RED] + a.1[FLR])));
        eprintln!("[REF-POT] top call-site owners (reducible / total):");
        for (owner, c) in rows.iter().take(25) {
            let t = c[RED] + c[FLR];
            let p = if t > 0 { c[RED] * 100 / t } else { 0 };
            eprintln!("[REF-POT]   {:>11} / {:<11} ({p:>3}% reducible)  {}", c[RED], t, owner);
        }
    }
}
