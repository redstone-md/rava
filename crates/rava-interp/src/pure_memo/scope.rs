//! The memo scope and what it admits.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::pure_memo::*;

/// RAII handle for one intercepted call. Opens (or joins) a window, serves a hit, and
/// records the result on a clean return.
pub(crate) struct Scope {
    key: Option<Key>,
    opened: bool,
}

impl Scope {
    /// `code` = resolved target `Code` pointer, `cells` = the raw operand cells.
    /// `memoizable` = the verdict said `Memo` AND every reference cell passed the
    /// key-stability test.
    pub(crate) fn enter(code: usize, cells: &[i64], memoizable: bool) -> Self {
        let depth = DEPTH.with(|d| {
            let v = d.get();
            d.set(v + 1);
            v
        });
        if depth == 0 {
            // Outermost window: start clean.
            TAINTED.with(|t| t.set(false));
            CACHE.with(|c| {
                let mut c = c.borrow_mut();
                // COUNTED, and counting it is the whole point: the `revalidate` clear was
                // instrumented first and reported `clears=1`, from which I concluded the keys
                // never repeat. That was the WRONG CLEAR SITE. This one runs at EVERY outermost
                // window — 12.7M of them on a boot — so the cache's entire lifetime is one
                // window's extent and cross-window reuse is impossible by construction.
                use std::sync::atomic::Ordering::Relaxed;
                tally::WINDOW_CLEARS.fetch_add(1, Relaxed);
                tally::WINDOW_CLEARED_ENTRIES.fetch_add(c.map.len() as u64, Relaxed);
                c.map.clear();
                c.stamp = epochs();
            });
        }
        let key = if memoizable && cells.len() <= MAX_CELLS {
            let mut k = Key { code, n: cells.len() as u8, cells: [0; MAX_CELLS] };
            k.cells[..cells.len()].copy_from_slice(cells);
            Some(k)
        } else {
            None
        };
        Scope { key, opened: depth == 0 }
    }

    /// A recorded result for this exact call, if the cache is still valid.
    pub(crate) fn lookup(&self) -> Option<i64> {
        let key = self.key?;
        let now = epochs();
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            c.revalidate(now);
            match c.map.get(&key).copied() {
                Some(v) => {
                    c.hits += 1;
                    Some(v)
                }
                None => {
                    c.misses += 1;
                    None
                }
            }
        })
    }

    /// Record a completed call's primitive result. Refused after a taint, and refused
    /// if a collection ran DURING the call (the key's addresses no longer denote the
    /// objects that produced this value).
    pub(crate) fn store(&self, bits: i64) {
        let Some(key) = self.key else { return };
        if TAINTED.with(|t| t.get()) {
            return;
        }
        let now = epochs();
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.stamp != now || c.map.len() >= MAX_ENTRIES {
                // A move happened inside the call, or the window is huge — fail open.
                c.revalidate(now);
                return;
            }
            c.map.insert(key, bits);
            c.stores += 1;
        });
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        if self.opened {
            TAINTED.with(|t| t.set(false));
            CACHE.with(|c| {
                let mut c = c.borrow_mut();
                let (h, m, s) = (c.hits, c.misses, c.stores);
                c.map.clear();
                c.hits = h;
                c.misses = m;
                c.stores = s;
            });
            stats_roll();
        }
    }
}

pub fn dump() {
    use std::sync::atomic::Ordering::Relaxed;
    if !armed() {
        return;
    }
    let (h, m, s, w) = (
        tally::HITS.load(Relaxed),
        tally::MISSES.load(Relaxed),
        tally::STORES.load(Relaxed),
        tally::WINDOWS.load(Relaxed),
    );
    let looked = h + m;
    let pct = if looked > 0 { h * 1000 / looked } else { 0 };
    eprintln!(
        "[PURE-MEMO] windows={w} lookups={looked} hits={h} ({}.{}%) stores={s} clears={} cleared_entries={} wclears={} wcleared={} — each hit is one guest call subtree not re-executed",
        pct / 10,
        pct % 10,
        tally::CLEARS.load(Relaxed),
        tally::CLEARED_ENTRIES.load(Relaxed),
        tally::WINDOW_CLEARS.load(Relaxed),
        tally::WINDOW_CLEARED_ENTRIES.load(Relaxed)
    );
    admission::dump();
}

// ─────────────────── descriptor shape (pure, unit-tested) ───────────────────

/// Local-SLOT layout of a call's operand buffer: `(reference_mask, slot_count)`,
/// receiver first for an instance call. `long`/`double` occupy two slots, only the
/// first of which is listed. `None` when the descriptor is malformed or the call
/// needs more slots than one key holds.
pub(crate) fn desc_slot_shape(desc: &str, is_static: bool) -> Option<(u8, u8)> {
    let b = desc.as_bytes();
    let mut i = desc.find('(')? + 1;
    let mut slots: u32 = 0;
    let mut mask: u32 = 0;
    if !is_static {
        mask |= 1; // receiver is always a reference
        slots = 1;
    }
    while i < b.len() && b[i] != b')' {
        let mut is_ref = false;
        while b.get(i) == Some(&b'[') {
            is_ref = true;
            i += 1;
        }
        // An ARRAY is a reference of one slot whatever its element type, so the `[`
        // prefix decides before the element letter does.
        let (width, r) = match (is_ref, b.get(i)?) {
            (true, b'L') => {
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                (1, true)
            }
            (true, _) => (1, true),
            (false, b'L') => {
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                (1, true)
            }
            (false, b'J' | b'D') => (2, false),
            (false, _) => (1, false),
        };
        if r {
            mask |= 1 << slots;
        }
        slots += width;
        i += 1;
        if slots as usize > MAX_CELLS {
            return None;
        }
    }
    Some((mask as u8, slots as u8))
}

pub(crate) mod admission {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static SEEN: Mutex<Option<HashMap<String, (super::Verdict, String)>>> = Mutex::new(None);

    pub(crate) fn note(key: String, v: super::Verdict, why: String) {
        start_watchdog();
        let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(HashMap::new).entry(key).or_insert((v, why));
    }

    /// PERIODIC DUMP (`RAVA_PURE_MEMO_DUMP_MS`, default 20 s once the probe is armed).
    ///
    /// The only caller of `pure_memo::dump()` is rava-cli's EXIT path, and a server boot never
    /// exits — the same trap that made a real-boot PGO profile unobtainable until a timer was
    /// added. So this census could only ever be read on a driver that returns from `main`,
    /// which is not the workload the "73% of ring computes are re-derived" claim is about.
    /// ★A census you cannot read on the workload you care about reports nothing, and reads
    /// exactly like a census that found nothing.
    ///
    /// Started from `note` rather than from `armed()`: `note` runs once per distinct
    /// (class, name, descriptor), while `armed()` is on the call path.
    pub(crate) fn start_watchdog() {
        static W: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        W.get_or_init(|| {
            let ms = std::env::var("RAVA_PURE_MEMO_DUMP_MS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(20_000)
                .max(1_000);
            let _ = std::thread::Builder::new()
                .name("rava-purememo-dump".to_string())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    super::dump();
                });
        });
    }

    pub(crate) fn dump() {
        let g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let Some(m) = g.as_ref() else { return };
        let mut rows: Vec<_> = m.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        let memo = rows.iter().filter(|(_, (v, _))| *v == super::Verdict::Memo).count();
        let open = rows.iter().filter(|(_, (v, _))| *v == super::Verdict::Opener).count();
        eprintln!(
            "[PURE-MEMO-ADMIT] {} dispatched methods examined: {memo} MEMO, {open} OPENER, {} refused",
            rows.len(),
            rows.len() - memo - open
        );
        for (k, (v, why)) in rows.iter().filter(|(_, (v, _))| *v != super::Verdict::No) {
            eprintln!("[PURE-MEMO-ADMIT]   {v:?}  {k}   {why}");
        }
        if !super::probe_enabled() {
            return;
        }
        for (k, (_, why)) in rows.iter().filter(|(_, (v, _))| *v == super::Verdict::No) {
            eprintln!("[PURE-MEMO-ADMIT]   no    {k}   {why}");
        }
    }
}
