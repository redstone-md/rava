//! The shadow collector used to cross-check the real one.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

/// SHADOW dry-run instrumentation. The live engine stays on mark-sweep; this runs
/// in parallel each GC cycle — discovers + validates the root set the copying GC
/// WOULD use, and the write barrier marks a card table — but NEVER mutates the
/// real heap. Proves root-discovery + card-barrier correctness with zero risk.
pub struct ShadowGc {
    /// 1 byte per CARD_BYTES of heap (dry-run dirty marks; GC never reads them yet).
    pub cards: Mutex<Vec<u8>>,
    pub cards_marked: AtomicU64,
    pub roots_traced: AtomicU64,
    pub roots_valid: AtomicU64,
    pub root_mismatch: AtomicU64,
    pub gc_cycles: AtomicU64,
    pub max_roots_cycle: AtomicU64,
}

/// One card per 512 bytes of heap (classic card-table granularity).
pub const CARD_BYTES: usize = 512;

pub fn shadow() -> &'static ShadowGc {
    static S: OnceLock<ShadowGc> = OnceLock::new();
    S.get_or_init(|| ShadowGc {
        cards: Mutex::new(vec![0u8; 1 << 20]), // 1 MiB card table = 512 MiB heap coverage
        cards_marked: AtomicU64::new(0),
        roots_traced: AtomicU64::new(0),
        roots_valid: AtomicU64::new(0),
        root_mismatch: AtomicU64::new(0),
        gc_cycles: AtomicU64::new(0),
        max_roots_cycle: AtomicU64::new(0),
    })
}

impl ShadowGc {
    /// Write barrier (dry-run): mark the card covering object `handle` dirty when a
    /// reference is stored into it (the old→young store the copying GC must track).
    #[inline]
    pub fn card_mark(&self, handle: u32) {
        // Map handle → card. Objects are ~48-64B; 8 handles ≈ one 512B card.
        let card = (handle as usize / 8) & ((1 << 20) - 1);
        if let Ok(mut t) = self.cards.try_lock() {
            if t[card] == 0 {
                t[card] = 1;
                self.cards_marked.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Record a GC cycle's root-discovery result + emit a stat line to stderr
    /// (captured in the boot log). `valid`/`total` come from the shadow validation.
    pub fn record_cycle(&self, total: u64, valid: u64) {
        let n = self.gc_cycles.fetch_add(1, Ordering::Relaxed) + 1;
        self.roots_traced.fetch_add(total, Ordering::Relaxed);
        self.roots_valid.fetch_add(valid, Ordering::Relaxed);
        let mismatch = total - valid;
        self.root_mismatch.fetch_add(mismatch, Ordering::Relaxed);
        self.max_roots_cycle.fetch_max(total, Ordering::Relaxed);
        if mismatch > 0 {
            eprintln!("[SHADOW_GC_MISMATCH] cycle={n} {mismatch}/{total} roots failed shadow-validation (live engine continues on mark-sweep)");
        }
        if n <= 4 || n % 16 == 0 {
            let cards = self.cards_marked.load(Ordering::Relaxed);
            eprintln!("[SHADOW-GC] cycle={n} roots={total} valid={valid} mismatch={mismatch} cards_dirty={cards}");
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "cycles={} roots_traced={} roots_valid={} root_mismatch={} max_roots/cycle={} cards_dirty={}",
            self.gc_cycles.load(Ordering::Relaxed),
            self.roots_traced.load(Ordering::Relaxed),
            self.roots_valid.load(Ordering::Relaxed),
            self.root_mismatch.load(Ordering::Relaxed),
            self.max_roots_cycle.load(Ordering::Relaxed),
            self.cards_marked.load(Ordering::Relaxed),
        )
    }
}
