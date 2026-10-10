//! Receiver-type profiles for tier 2, read from tier-1 inline caches (per method and bytecode
//! index, as HotSpot's method data is): a call site whose cache held one class (or two, for the
//! optimizing tier) is inlined for those classes behind exact class checks.

use super::runtime::IcCell;
use crate::klass::Klass;
use crate::method::Method;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

fn sites() -> &'static Mutex<HashMap<(usize, u32), &'static IcCell>> {
    static SITES: OnceLock<Mutex<HashMap<(usize, u32), &'static IcCell>>> = OnceLock::new();
    SITES.get_or_init(Default::default)
}

/// The inline cache of `m`'s call at `bci`, shared by every compiled copy of that call (inlined
/// ones and optimized code included), so the site has one receiver profile as HotSpot's method
/// data has.
pub(super) fn cell(m: &'static Method, bci: usize) -> &'static IcCell {
    *sites().lock().unwrap().entry((m as *const Method as usize, bci as u32)).or_insert_with(IcCell::new)
}

fn cast_sites() -> &'static Mutex<HashMap<(usize, u32), &'static std::sync::atomic::AtomicUsize>> {
    static SITES: OnceLock<Mutex<HashMap<(usize, u32), &'static std::sync::atomic::AtomicUsize>>> = OnceLock::new();
    SITES.get_or_init(Default::default)
}

/// The class that last passed `m`'s interface type check at `bci` in the runtime (first-tier
/// code checks it inline and the runtime records it: HotSpot's profile of a cast's classes).
pub(super) fn cast_cell(m: &'static Method, bci: usize) -> &'static std::sync::atomic::AtomicUsize {
    *cast_sites().lock().unwrap().entry((m as *const Method as usize, bci as u32)).or_insert_with(|| Box::leak(Box::default()))
}

/// The class `cast_cell` holds for `m`'s type check at `bci`, if any.
pub(super) fn cast_seen(m: &'static Method, bci: usize) -> Option<&'static Klass> {
    let cell = *cast_sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32))?;
    let k = cell.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: recorded classes are immortal.
    (k != 0).then(|| unsafe { &*(k as *const Klass) })
}

/// The receiver classes seen at `m`'s call at `bci`, when there were one or two.
pub(super) fn receivers(m: &'static Method, bci: usize) -> Vec<&'static Klass> {
    let Some(cell) = sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32)).copied() else { return Vec::new() };
    // SAFETY: cached classes are immortal.
    cell.receivers().unwrap_or_default().into_iter().map(|k| unsafe { &*(k as *const Klass) }).collect()
}

/// The class at least 90% of the receivers counted at `m`'s call at `bci` had, if any.
pub(super) fn major_receiver(m: &'static Method, bci: usize) -> Option<&'static Klass> {
    let cell = *sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32))?;
    // SAFETY: cached classes are immortal.
    cell.major().map(|k| unsafe { &*(k as *const Klass) })
}

/// The recorded receiver classes that each took a large share of the calls counted at `m`'s
/// call at `bci`, most frequent first (`IcCell::frequent`).
pub(super) fn frequent_receivers(m: &'static Method, bci: usize) -> Vec<&'static Klass> {
    let Some(cell) = sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32)).copied() else { return Vec::new() };
    // SAFETY: cached classes are immortal.
    cell.frequent().into_iter().map(|k| unsafe { &*(k as *const Klass) }).collect()
}

fn hot_sites() -> &'static std::sync::Mutex<std::collections::HashSet<(usize, u32)>> {
    static S: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<(usize, u32)>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Marks `m`'s call at `bci` as run often from optimized code that judged it cold.
pub(crate) fn mark_hot_site(m: &'static Method, bci: usize) {
    hot_sites().lock().unwrap().insert((m as *const Method as usize, bci as u32));
}

/// Whether `m`'s call at `bci` turned out hot after being compiled as a cold call.
pub(crate) fn hot_site(m: &'static Method, bci: usize) -> bool {
    hot_sites().lock().unwrap().contains(&(m as *const Method as usize, bci as u32))
}

/// Whether `m`'s call at `bci` has recorded no receiver class at all (never run by profiling
/// code before this compilation).
pub(crate) fn unprofiled(m: &'static Method, bci: usize) -> bool {
    sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32)).is_none_or(|cell| cell.receivers().is_some_and(|r| r.is_empty()))
}

/// Whether `m`'s call at `bci` has seen more receiver classes than its profile keeps.
pub(crate) fn megamorphic(m: &'static Method, bci: usize) -> bool {
    sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32)).is_some_and(|cell| cell.receivers().is_none())
}

/// The only receiver class seen at `m`'s call at `bci`, if exactly one was.
pub(super) fn monomorphic(m: &'static Method, bci: usize) -> Option<&'static Klass> {
    let cell = *sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32))?;
    // SAFETY: cached classes are immortal.
    cell.observed().map(|k| unsafe { &*(k as *const Klass) })
}

/// Adds `k` to the receivers seen at `m`'s call at `bci` (optimized code that deoptimized on
/// a receiver its profile did not have, so the recompilation knows it).
pub(crate) fn note_receiver(m: &'static Method, bci: usize, k: &'static Klass) {
    if let Some(cell) = sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32)).copied() {
        cell.note(k as *const Klass as usize);
    }
}

/// What the profile of `m`'s call at `bci` holds, for diagnostics.
pub(crate) fn describe(m: &'static Method, bci: usize) -> String {
    let Some(cell) = sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32)).copied() else { return "no profile".into() };
    match cell.receivers() {
        None => {
            let (a, b, o) = cell.shares();
            format!("megamorphic {a}/{b}/{o}")
        }
        Some(v) => format!("{} receivers", v.len()),
    }
}

fn traps() -> &'static Mutex<HashSet<(usize, u32)>> {
    static TRAPS: OnceLock<Mutex<HashSet<(usize, u32)>>> = OnceLock::new();
    TRAPS.get_or_init(Default::default)
}

/// Records that optimized code left for the interpreter at `m`'s instruction `bci` (HotSpot's
/// per-bytecode trap history): recompilations do not speculate there again.
pub(crate) fn note_trap(m: &'static Method, bci: usize) {
    traps().lock().unwrap().insert((m as *const Method as usize, bci as u32));
}

/// Whether optimized code ever deoptimized at `m`'s instruction `bci`.
pub(crate) fn trapped(m: &'static Method, bci: usize) -> bool {
    traps().lock().unwrap().contains(&(m as *const Method as usize, bci as u32))
}

/// Executions of a conditional branch by baseline code: taken, then not taken (HotSpot's
/// `BranchData`).
#[derive(Default)]
pub struct BranchCell {
    pub counts: [std::sync::atomic::AtomicU32; 2],
}

fn branches() -> &'static Mutex<HashMap<(usize, u32), &'static BranchCell>> {
    static BRANCHES: OnceLock<Mutex<HashMap<(usize, u32), &'static BranchCell>>> = OnceLock::new();
    BRANCHES.get_or_init(Default::default)
}

/// The counters of `m`'s branch at `bci`, created on first use (in low memory: compiled code
/// increments them through 32-bit absolute addresses).
pub(super) fn branch_cell(m: &'static Method, bci: usize) -> &'static BranchCell {
    branches().lock().unwrap().entry((m as *const Method as usize, bci as u32)).or_insert_with(|| crate::low::leak(BranchCell::default()))
}

/// (taken, not taken) counts of `m`'s branch at `bci`, if baseline code profiled it.
pub(super) fn branch_counts(m: &'static Method, bci: usize) -> Option<(u32, u32)> {
    use std::sync::atomic::Ordering;
    let cell = *branches().lock().unwrap().get(&(m as *const Method as usize, bci as u32))?;
    Some((cell.counts[0].load(Ordering::Relaxed), cell.counts[1].load(Ordering::Relaxed)))
}
