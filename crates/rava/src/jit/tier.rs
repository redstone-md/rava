//! Tiered recompilation. Tier-1 code counts invocations (in the prologue) and taken back-edges.
//! A hot method is compiled again at tier 2, which inlines hot callees with a larger budget
//! (HotSpot's `FreqInlineSize`). A loop still running tier-1 code moves into the tier-2 version
//! at its next back-edge: both versions share the frame layout, the operand stack is in memory
//! at a loop header, and the tier-2 entry stub for that block reloads the pinned locals and
//! claims its own frame size.

use super::asm::{mem, Alu, Cond, R};
use super::compile::Emitter;
use crate::method::Method;
use crate::thread::Thread;
use std::sync::atomic::Ordering;

/// Invocations and taken back-edges of tier-1 code before tier 2 (cf. HotSpot's
/// `Tier4InvocationThreshold` / `Tier4BackEdgeThreshold`); `RAVA_TIER2_THRESHOLD` sets the
/// invocation count (back-edges scale with it). Half HotSpot's 5000: code compiled later is
/// compiled from fuller profiles, and with today's compiler that pays (All The Mods 10, 250 cows
/// and 100 villagers: 1000 33.65 ms/tick, 2500 32.4, 5000 32.5 over four and two runs; start
/// no slower, CPU -10%; a Paper start 7.6 -> 7.2 s, CPU 36 -> 28 s). Earlier, with a slower
/// compiler, 1000 had run the first sprint 13% faster than 1500.
fn thresholds() -> (i32, i32) {
    static T: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        let n = std::env::var("RAVA_TIER2_THRESHOLD").ok().and_then(|v| v.parse::<i32>().ok()).unwrap_or(2500).max(1);
        (n, n * 8)
    })
}

/// Counts between requests once a counter is past its threshold (a power of two).
pub(super) const RETRY: i32 = 1024;

/// The invocation threshold tier-1 prologues count toward, unless tier 2 is off.
pub(super) fn invocation_threshold() -> Option<i32> {
    enabled().then(|| thresholds().0)
}

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_TIER2").is_none())
}

/// Requests the tier-2 compilation of `m` once. Tier-1 code calls this on every invocation
/// from the threshold on (the interpreter may have counted past it while the method waited to
/// be compiled), so a request already made costs a load.
pub extern "sysv64" fn tier_up(m: *const Method) {
    // SAFETY: generated code passes a live method.
    let m: &'static Method = unsafe { &*m };
    if m.tiered.load(Ordering::Acquire) || m.compiled().is_some_and(|c| c.tier >= 2) || m.tiered.swap(true, Ordering::AcqRel) {
        return;
    }
    super::queue::submit(super::queue::Job::TierUp(m));
}

/// Compiles `m` at tier 2: optimized, else baseline with the larger inlining budget.
pub(super) fn compile_tier2(m: &'static Method) {
    let optimized = super::opt::enabled().then(|| super::opt::compile(m)).flatten();
    if let (Some(s), true) = (super::stats::get(), optimized.is_some()) {
        s.opt_methods.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(cm) = optimized.or_else(|| super::compile::compile(m, 2)) {
        if let Some(s) = super::stats::get() {
            s.tier2_methods.fetch_add(1, Ordering::Relaxed);
        }
        m.install(cm);
    }
}

/// A hot loop in tier-1 code: tier `m` up and return the tier-2 address of block `bci` for the
/// frame at `locals`, or 0 to stay (no tier-2 code, or no room for its larger frame).
pub extern "sysv64" fn tier_up_osr(t: *mut Thread, m: *const Method, bci: u64, locals: *mut u64) -> usize {
    tier_up(m);
    // SAFETY: the current thread and a live method.
    let (t, m): (_, &'static Method) = unsafe { (&*t, &*m) };
    let room = (t.stack_limit() as usize).saturating_sub(locals as usize) / 8;
    // Baseline tier-2 code has block entries; optimized code is entered through a version
    // compiled for this loop (`opt/osr.rs`), requested now while the method's own tier-2
    // compilation may still be under way.
    let block = m.compiled().filter(|c| c.tier >= 2 && c.frame_slots <= room).and_then(|cm| cm.address_of(bci as u32));
    if let Some(address) = super::opt::osr::entry(m, bci as u32, room).or(block) {
        return address;
    }
    super::opt::osr::request(m, bci as u32);
    // Ask again after another threshold's worth of back-edges.
    m.backedges.store(0, Ordering::Relaxed);
    0
}

impl Emitter {
    /// From the threshold on, the counter at rax asks only once every `RETRY` counts: tier-1
    /// code that stays (its tier-2 compilation pending or failed, or an inlined loop's method)
    /// would otherwise call the runtime on every invocation or back-edge.
    fn past_threshold(&mut self, threshold: i32, cont: super::asm::Label) {
        self.a.load(false, R::Rdx, mem(R::Rax, 0));
        self.a.alu_ri(false, Alu::Sub, R::Rdx, threshold);
        self.a.alu_ri(false, Alu::And, R::Rdx, RETRY - 1);
        self.a.jcc(Cond::Ne, cont);
    }

    /// A taken back-edge to `target` in tier-1 code (the stack already written back): counts
    /// it and, once hot, continues in tier-2 code when this is the method's own frame.
    pub(super) fn count_backedge(&mut self, target: usize) {
        if self.tier >= 2 || !enabled() {
            return;
        }
        let cont = self.a.new_label();
        self.a.mov_ri(R::Rax, &self.m.backedges as *const _ as i64);
        self.a.alu_mi(false, Alu::Add, mem(R::Rax, 0), 1);
        self.a.alu_mi(false, Alu::Cmp, mem(R::Rax, 0), thresholds().1);
        self.a.jcc(Cond::L, cont);
        self.past_threshold(thresholds().1, cont);
        if self.inl.is_some() {
            // An inlined loop has no frame of its own to move: just recompile its method.
            self.a.mov_ri(R::Rdi, self.m as *const Method as i64);
            self.a.call_abs(tier_up as *const () as usize);
        } else {
            self.a.mov_rr(true, R::Rdi, R::R12);
            self.a.mov_ri(R::Rsi, self.m as *const Method as i64);
            self.a.mov_ri(R::Rdx, target as i64);
            self.a.lea(R::Rcx, super::compile::slot(0));
            self.a.call_abs(tier_up_osr as *const () as usize);
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::E, cont);
            self.a.jmp_r(R::Rax);
        }
        self.a.bind(cont);
    }
}
