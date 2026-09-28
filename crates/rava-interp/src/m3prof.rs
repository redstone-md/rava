//! M3 9.1 WORLDGEN PROFILE PROBE (RAVA_M3_PROFILE_PROBE). A read-only inclusive/self-time call
//! profiler hooked at `Vm::invoke` (the universal method entry — compiled subtrees run to completion
//! inside it, so inclusive time is correct). Attributes wall-time to worldgen top-level OOP entry
//! points, classifies each into a phase (shape-cache vs biome/noise worldgen), and statically counts
//! per-method state-mutation markers (putfield = COW-isolatable; putstatic / array-store = shared,
//! hard to isolate) — the semantic-isolation signal for choosing an M3 fork point. Gated, off ⇒ zero
//! effect. Does NOT touch the executor or GIL.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;

const DEPTH_CAP: usize = 24; // bound per-call work on deep worldgen recursion

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M3_PROFILE_PROBE").is_some())
}

/// Worldgen phase for a class name, or None (not profiled).
fn phase_of(class: &str) -> Option<&'static str> {
    let l = class.to_ascii_lowercase();
    if l.contains("blockbehaviour")
        || l.contains("statedefinition")
        || l.contains("stateholder")
        || l.contains("block/state")
        || l.contains("blockstate")
    {
        return Some("shapecache");
    }
    if l.contains("levelgen")
        || l.contains("/biome")
        || l.contains("densityfunction")
        || l.contains("/synth/")
        || l.contains("noise")
        || l.contains("climate")
        || l.contains("/chunk")
        || l.contains("carver")
        || l.contains("heightmap")
    {
        return Some("worldgen");
    }
    None
}

#[derive(Default, Clone, Copy)]
pub struct Stat {
    pub calls: u64,
    pub incl_ns: u128,
    pub self_ns: u128,
    pub top_incl_ns: u128, // inclusive time when this frame was the OUTERMOST profiled frame
    pub phase: &'static str,
    pub putstatic: u32,
    pub arraystore: u32,
    pub putfield: u32,
}

struct Fr {
    key: String,
    start: Instant,
    child: u128,
    top: bool,
}

thread_local! {
    static STACK: RefCell<Vec<Fr>> = const { RefCell::new(Vec::new()) };
}
static STATS: Mutex<Option<HashMap<String, Stat>>> = Mutex::new(None);
static T0: OnceLock<Instant> = OnceLock::new();

/// Static mutation-marker scan (once per method): putfield vs putstatic/array-store counts.
fn scan_markers(bc: &[u8]) -> (u32, u32, u32) {
    let (mut ps, mut asr, mut pf) = (0u32, 0u32, 0u32);
    let mut pc = 0;
    while pc < bc.len() {
        match bc[pc] {
            0xb3 => ps += 1,                                     // putstatic
            0xb5 => pf += 1,                                     // putfield
            0x4f..=0x56 => asr += 1,                             // *astore (array element store)
            _ => {}
        }
        match crate::jit::insn_len_at(bc, pc) {
            Some(l) => pc += l,
            None => break,
        }
    }
    (ps, asr, pf)
}

/// Enter a profiled method. Returns true iff a frame was pushed (caller must pair with `exit`).
pub fn enter(class: &str, name: &str, bc: &[u8]) -> bool {
    let Some(phase) = phase_of(class) else { return false };
    T0.get_or_init(Instant::now);
    let depth = STACK.with(|s| s.borrow().len());
    if depth >= DEPTH_CAP {
        return false; // too deep — time rolls into the depth-cap ancestor's self
    }
    let top = depth == 0;
    let key = format!("{class}#{name}");
    // Record the static markers + phase once (calls counted at exit).
    {
        let mut g = STATS.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        let e = m.entry(key.clone()).or_default();
        if e.phase.is_empty() {
            e.phase = phase;
            let (ps, asr, pf) = scan_markers(bc);
            e.putstatic = ps;
            e.arraystore = asr;
            e.putfield = pf;
        }
    }
    STACK.with(|s| s.borrow_mut().push(Fr { key, start: Instant::now(), child: 0, top }));
    true
}

pub fn exit() {
    let fr = STACK.with(|s| s.borrow_mut().pop());
    let Some(fr) = fr else { return };
    let incl = fr.start.elapsed().as_nanos();
    let self_ns = incl.saturating_sub(fr.child);
    STACK.with(|s| {
        if let Some(p) = s.borrow_mut().last_mut() {
            p.child += incl;
        }
    });
    let mut g = STATS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = g.as_mut() {
        let e = m.entry(fr.key).or_default();
        e.calls += 1;
        e.incl_ns += incl;
        e.self_ns += self_ns;
        if fr.top {
            e.top_incl_ns += incl;
        }
    }
}

/// Aggregate report: phase self-time split, top methods by self-time, isolation markers.
pub fn dump() {
    let boot_ns = T0.get().map(|t| t.elapsed().as_nanos()).unwrap_or(0);
    let g = STATS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = g.as_ref() else {
        eprintln!("[M3-PROFILE] no worldgen methods recorded");
        return;
    };
    let ms = |ns: u128| (ns / 1_000_000) as u64;
    let pct = |ns: u128| if boot_ns > 0 { (ns * 100 / boot_ns) as u64 } else { 0 };
    // Phase self-time split (each ns belongs to exactly one method → one phase).
    let (mut shape_self, mut wg_self) = (0u128, 0u128);
    for st in m.values() {
        match st.phase {
            "shapecache" => shape_self += st.self_ns,
            "worldgen" => wg_self += st.self_ns,
            _ => {}
        }
    }
    eprintln!(
        "[M3-PROFILE] boot={}ms | profiled-self: shape-cache={}ms ({}%) worldgen={}ms ({}%) | rest=JDK/bootstrap/other",
        ms(boot_ns), ms(shape_self), pct(shape_self), ms(wg_self), pct(wg_self)
    );
    // Top methods by self-time, with isolation markers.
    let mut v: Vec<(&String, &Stat)> = m.iter().collect();
    v.sort_by(|a, b| b.1.self_ns.cmp(&a.1.self_ns));
    eprintln!("[M3-PROFILE] top worldgen entry points by SELF time (fork-point candidates):");
    eprintln!("[M3-PROFILE]   self_ms  incl_ms  top_incl_ms   calls  phase       putfield putstatic arraystore  method");
    for (k, st) in v.iter().take(20) {
        eprintln!(
            "[M3-PROFILE]   {:>7} {:>8} {:>12} {:>7} {:>-11} {:>8} {:>9} {:>10}  {}",
            ms(st.self_ns), ms(st.incl_ns), ms(st.top_incl_ns), st.calls, st.phase,
            st.putfield, st.putstatic, st.arraystore, k
        );
    }
}
