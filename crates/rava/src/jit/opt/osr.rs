//! On-stack replacement into optimized code (HotSpot's OSR compilations): a loop still running
//! first-tier code once its method has optimized code (which has no block entries) moves into a
//! version of the method compiled from the loop's header. That version reads the live locals
//! from the frame's slots (memory is current at a back edge, `vstack.rs`) as parameters, builds
//! only what the header reaches, and is called from a bridge at its start (`codegen.rs`) that
//! then leaves the method as first-tier code would. A long-running loop of a method called once
//! (module graph resolution, data loading) otherwise ran first-tier code to its end.
//!
//! Each method keeps its OSR versions in a list (newest first, never freed, as all code); one
//! that keeps deoptimizing is retired and compiled again, up to `MAX_VERSIONS` per loop.

use super::ir::Ty;
use crate::jit::analysis::{insn_len, successors};
use crate::method::Method;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// `OsrCode::entry` while its compilation is queued or running.
const PENDING: usize = 0;
/// The method cannot be compiled from this loop: not tried again.
const REFUSED: usize = 1;
/// Deoptimized too often: compiled again on the next request.
const RETIRED: usize = 2;

const DEOPTS_BEFORE_RETIRE: u32 = 8;
const MAX_VERSIONS: usize = 4;

pub(crate) struct OsrCode {
    bci: u32,
    /// The bridge's address once compiled, else one of the states above.
    entry: AtomicUsize,
    /// Value-stack slots the compiled frame uses (inlined frames included).
    frame_slots: AtomicUsize,
    deopts: AtomicU32,
    next: Option<&'static OsrCode>,
}

impl OsrCode {
    /// Links an unpublished version in front of `next` (`Method::push_osr`).
    pub(crate) fn set_next(&mut self, next: Option<&'static OsrCode>) {
        self.next = next;
    }
}

fn versions(m: &Method, bci: u32) -> impl Iterator<Item = &'static OsrCode> {
    std::iter::successors(m.osr_head(), |c| c.next).filter(move |c| c.bci == bci)
}

/// The entry of `m`'s optimized code for the loop at `bci`, if compiled and its frame fits in
/// `room` slots.
pub(crate) fn entry(m: &Method, bci: u32, room: usize) -> Option<usize> {
    let c = versions(m, bci).next()?;
    let entry = c.entry.load(Ordering::Acquire);
    (entry > RETIRED && c.frame_slots.load(Ordering::Relaxed) <= room).then_some(entry)
}

/// Queues the compilation of `m` from the loop at `bci`, unless one is queued, done, refused or
/// retired too often.
pub(crate) fn request(m: &'static Method, bci: u32) {
    // A synchronized method's monitor is held by the frame's first-tier code, which the
    // optimized version would not know to release on deoptimization.
    if !super::enabled() || m.is_synchronized() || m.not_compilable.load(Ordering::Relaxed) {
        return;
    }
    if versions(m, bci).next().is_some_and(|c| c.entry.load(Ordering::Acquire) != RETIRED) || versions(m, bci).count() >= MAX_VERSIONS {
        return;
    }
    let c = OsrCode { bci, entry: AtomicUsize::new(PENDING), frame_slots: AtomicUsize::new(0), deopts: AtomicU32::new(0), next: None };
    crate::jit::queue::submit(crate::jit::queue::Job::Osr(m, m.push_osr(c)));
}

/// Compiles the version `c` of `m` (a queued request).
pub(in crate::jit) fn compile(m: &'static Method, c: &'static OsrCode) {
    match super::compile_osr(m, c.bci) {
        Some(cm) => {
            c.frame_slots.store(cm.frame_slots, Ordering::Relaxed);
            c.entry.store(cm.entry, Ordering::Release);
        }
        None => c.entry.store(REFUSED, Ordering::Release),
    }
}

/// Counts a deoptimization of the version of `m` for the loop at `bci`.
pub(super) fn note_deopt(m: &Method, bci: u32) {
    if let Some(c) = versions(m, bci).next().filter(|c| c.entry.load(Ordering::Acquire) > RETIRED) {
        if c.deopts.fetch_add(1, Ordering::Relaxed) + 1 == DEOPTS_BEFORE_RETIRE {
            c.entry.store(RETIRED, Ordering::Release);
        }
    }
}

/// The kind of each local of `m` at `bci` over every path from the method's entry, as the
/// verifier infers it (`None`: unset there, or of different kinds on different paths); `None`
/// for a method with subroutines (`jsr`/`ret`), whose locals have no single kind.
pub(super) fn local_types(m: &Method, bci: usize) -> Option<Vec<Option<Ty>>> {
    let info = m.code?;
    let code = info.code;
    let mut entry = vec![None; info.max_locals as usize];
    let mut slot = 0;
    if !m.is_static() {
        entry[0] = Some(Ty::A);
        slot = 1;
    }
    for p in m.sig.params.iter() {
        entry[slot] = Some(Ty::of(*p));
        slot += p.slots() as usize;
    }
    let mut state: Vec<Option<Vec<Option<Ty>>>> = vec![None; code.len()];
    state[0] = Some(entry);
    let mut work = vec![0];
    while let Some(pc) = work.pop() {
        let before = state[pc].clone().expect("a reached instruction");
        let mut after = before.clone();
        if !store(code, pc, &mut after) {
            return None;
        }
        let len = insn_len(code, pc);
        let handlers = info.handlers.iter().filter(|h| (h.start as usize..h.end as usize).contains(&pc)).map(|h| h.handler as usize);
        // A handler is entered from before or after any instruction it covers.
        let edges = successors(code, pc, len).into_iter().map(|s| (s, &after)).chain(handlers.flat_map(|h| [(h, &before), (h, &after)]));
        for (to, s) in edges.filter(|&(to, _)| to < code.len()) {
            let changed = match &mut state[to] {
                Some(cur) => cur.iter_mut().zip(s).fold(false, |changed, (c, &n)| {
                    let differs = c.is_some() && *c != n;
                    if differs {
                        *c = None;
                    }
                    changed | differs
                }),
                none => {
                    *none = Some(s.clone());
                    true
                }
            };
            if changed {
                work.push(to);
            }
        }
    }
    state[bci].take()
}

/// Applies the local store at `pc` (if it is one) to `s`; false for a subroutine instruction.
fn store(code: &[u8], pc: usize, s: &mut [Option<Ty>]) -> bool {
    let (op, index) = match code[pc] {
        0xc4 => (code[pc + 1], u16::from_be_bytes([code[pc + 2], code[pc + 3]]) as usize),
        op => (op, code.get(pc + 1).copied().unwrap_or(0) as usize),
    };
    const KINDS: [Ty; 5] = [Ty::I, Ty::J, Ty::F, Ty::D, Ty::A];
    let (n, ty) = match op {
        0xa8 | 0xa9 | 0xc9 => return false,
        0x36..=0x3a => (index, KINDS[(op - 0x36) as usize]),
        0x3b..=0x4e => (((op - 0x3b) % 4) as usize, KINDS[((op - 0x3b) / 4) as usize]),
        _ => return true,
    };
    s[n] = Some(ty);
    if ty.wide() {
        s[n + 1] = None;
    }
    // The second half of a long or double before it is gone.
    if n > 0 && s[n - 1].is_some_and(Ty::wide) {
        s[n - 1] = None;
    }
    true
}
