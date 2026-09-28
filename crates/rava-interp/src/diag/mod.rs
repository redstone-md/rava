//! # Rava diagnostics registry
//!
//! One place to declare a counter or a histogram; one selector to arm them; one dump that fires
//! no matter how the process ends.
//!
//! ## Why this exists
//!
//! The engine already had ~30 instruments. Every one had its own env var, its own
//! `static AtomicU64`, and its own print at a site that seemed reasonable when it was written.
//! Seven times in one optimisation session an instrument existed, was armed, and printed
//! nothing — because it printed from a path that particular run never reaches:
//!
//! * `lane_probe` — a boot measured to `Done` and then killed never gets there;
//! * the `pc_profile` dump — only on a profiler build, and not in every configuration;
//! * the `run.rs` tail — after guest `main` RETURNS, which a probe does and a Minecraft server
//!   does not: it ends inside `System.exit` on a shutdown thread.
//!
//! **A diagnostic that depends on how the program ends is not a diagnostic.** So `dump` here is
//! wired to independent triggers and any one of them suffices.
//!
//! ## Cost when off
//!
//! A `Counter` carries its own `armed` bool, copied out of the selector at registration. The
//! armed path is one predictable branch plus a relaxed `fetch_add`; the disarmed path is the
//! branch alone. Registration happens once per site behind a `OnceLock`, so the hot path never
//! touches the registry.
//!
//! ⚠**AND THAT PARAGRAPH WAS TRUE OF THE INSTRUMENT AND FALSE OF THE PATH.** The branch on
//! `armed` really is the whole cost *once you hold the handle* — but every site REACHES the
//! handle through the `OnceLock` the macro plants, and `OnceLock::get_or_init` opens with an
//! ACQUIRE load. An acquire load is an optimisation barrier LLVM may not hoist out of the
//! caller's loop, may not CSE against the identical load three lines below it, and across which
//! it must keep the caller's state materialised. So a disarmed instrument cost the caller a
//! barrier, two handle loads and two branches — per instrument, not per function. `heap/arrays.rs`
//! pays that seven times for one array birth; `nextgen/tlab.rs` had to hand-fold four of them out
//! of the object-birth gate to get the bump back, and wrote the diagnosis down at `fast_birth3`.
//!
//! See `none_armed` for the fix: one hoistable, CSE-able byte in front of the WHOLE registry, and
//! a handle that is a compile-time constant when nothing is armed, so `armed` folds to `false`
//! and the caller's `inc()` / `start()` / `record()` disappears outright.
//!
//! ## Selecting
//!
//! ```text
//! RAVA_DIAG=all                  everything
//! RAVA_DIAG=jit                  every instrument whose category is `jit`
//! RAVA_DIAG=jit.compiles,mapkey  one instrument, and a whole category
//! RAVA_DIAG_OUT=stderr|<path>    where the dump goes (default stderr)
//! RAVA_DIAG_FORMAT=text|json     how it is written (default text)
//! RAVA_DIAG_EVERY_MS=<n>         also dump every n ms from a timer thread
//! RAVA_DIAG_EXIT_AFTER_MS=<n>    dump at n ms and exit, for measuring one phase
//! RAVA_NO_FAST_DIAG=1            revert the framework's zero-cost-when-disarmed shape
//! ```

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

mod meter;
pub use meter::{process_start, Meter, MeterGuard};

#[derive(Copy, Clone, PartialEq, Eq)]
enum Kind {
    Count,
    Hist,
    /// A METER: how much the population COSTS, not how big it is. See `meter.rs` for why the
    /// registry needed one and what it cannot tell you.
    Meter,
}

struct Entry {
    category: &'static str,
    name: &'static str,
    kind: Kind,
    cells: &'static [AtomicU64],
    /// One line of prose, printed beside the row. This is what makes a dump readable by someone
    /// who did not write the instrument, so it is required, not optional.
    help: &'static str,
}

fn registry() -> &'static Mutex<Vec<Entry>> {
    static R: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

fn selector() -> &'static Vec<String> {
    static S: OnceLock<Vec<String>> = OnceLock::new();
    S.get_or_init(|| {
        let mut selectors: Vec<String> = std::env::var("RAVA_DIAG")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if matches!(std::env::var("RAVA_DIAG_AOT_ROUTE").as_deref(), Ok("1"))
            && !selectors.iter().any(|selector| selector == "aot_route")
        {
            selectors.push("aot_route".to_owned());
        }
        selectors
    })
}

/// Is `category.name` selected? `all` selects everything; a bare category selects its members.
pub fn selected(category: &str, name: &str) -> bool {
    let sel = selector();
    if sel.is_empty() {
        return false;
    }
    let full = format!("{category}.{name}");
    sel.iter().any(|s| s == "all" || s == category || s == &full)
}

/// Anything armed at all? Used to skip work that only feeds instruments.
pub fn any_armed() -> bool {
    !selector().is_empty()
}

// ═══════ FAST DIAG — ONE HOISTABLE BYTE IN FRONT OF THE WHOLE REGISTRY (`RAVA_NO_FAST_DIAG=1`) ═══════
//
// **WHAT THE SHAPE WAS.** `diag_counter!` / `diag_meter!` / `diag_histogram!` each expand to a
// function whose body is `OnceLock::get_or_init`. Holding the handle, the instruments really are
// as cheap as the header claims: `Counter::add` is a branch, `Meter::start`'s disarmed arm is a
// branch and a guard that SROA deletes. Reaching the handle is not:
//
//   * `get_or_init` opens with an **acquire** load. On x86 that is a `mov`, and the `mov` is not
//     the cost — LLVM may not hoist it out of the caller's loop, may not CSE it against the
//     identical load at the next instrument in the same function, and must keep the caller's
//     values materialised across it;
//   * the handle is then two loads (cell pointer, `armed` byte) and a branch that LLVM cannot
//     fold, because `armed` is runtime data, not a constant.
//
// Per instrument. `heap/arrays.rs` runs that seven times per array birth (two counters, one of
// six kind-counters, one of two price meters); `vm/jit_call_body.rs` reads three; `nextgen/tlab.rs`
// gave up and hand-folded its four into a process-constant `OnceLock<bool>` (`fast_birth3`),
// which is the fix, done once, for one site.
//
// **WHAT THE SHAPE IS.** `RAVA_DIAG` is read once from the environment and never changes, so the
// answer to *is anything armed at all* is fixed for the process. `none_armed()` caches it in a
// const-initialised, destructor-free `thread_local` byte — a PLAIN load, which LLVM is free to
// hoist out of loops and to CSE across every instrument in a function, unlike the acquire loads it
// replaces. When it says yes, the macro returns a handle built from a `static` address and a
// literal `false`, so `armed` folds at compile time and the entire instrument — the branch, the
// `fetch_add`, the `MeterGuard` and its `Drop` — is deleted from the caller.
//
// Cost accounting, honestly: on Windows the TLS access is three dependent loads against the one
// acquire load it replaces, so a function containing exactly ONE disarmed instrument called once
// is roughly a wash. Everything else — two or more instruments in a function, any instrument in a
// loop, and every `.armed()` gate that other rounds folded by hand — strictly wins, and no
// barrier remains for the surrounding code to be pinned against.
//
// ⚠**THIS DOES NOT DISARM ANYTHING.** With any selector set, `none_armed()` is false and every
// site takes exactly the path it took before, including for instruments that selector left
// disarmed. The fast answer exists only for the run where NOTHING is armed, which is every
// product boot.

/// The round's kill switch. `RAVA_NO_FAST_DIAG=1` (or the blanket `RAVA_NO_EMIT_MORE`) restores
/// the previous shape everywhere: per-site `OnceLock` on every fetch, and a registry row plus
/// leaked cells for instruments nobody asked for.
fn fast_diag() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_DIAG").is_none() && !crate::flags::emit_more_disabled()
    })
}

thread_local! {
    /// 0 = not yet asked on this thread, 1 = nothing is armed, 2 = something is. `Cell<u8>`, const
    /// initialised and with no `Drop`, which is the only shape that lowers to a single TLS-slot
    /// load — a `RefCell`, a `Vec` or a destructor would put an out-of-line `LocalKey::with` and a
    /// lazy-init check on this path and undo the whole point.
    static NONE_ARMED: Cell<u8> = const { Cell::new(0) };
}

/// True when no instrument in the registry is armed, so a caller may use a constant disarmed
/// handle and let the optimiser delete the instrument. Answered from one plain byte after the
/// first call on a thread.
#[inline(always)]
pub fn none_armed() -> bool {
    match NONE_ARMED.with(|c| c.get()) {
        1 => true,
        2 => false,
        _ => none_armed_slow(),
    }
}

/// Once per thread. Out of line and `cold` so the byte test above stays three instructions at
/// every one of the ~90 sites that inline it.
#[cold]
#[inline(never)]
fn none_armed_slow() -> bool {
    let none = fast_diag() && !any_armed();
    NONE_ARMED.with(|c| c.set(if none { 1 } else { 2 }));
    none
}

/// Zero cells for every instrument nobody armed: one array in BSS, never written, shared by all
/// of them. A disarmed instrument's cells were previously a `Vec` built element by element and
/// leaked — 8 bytes for a counter, 272 for a histogram and **2,072 for a meter**, times every
/// declared instrument, for storage whose only reader (`render`) filters it out again.
static ZERO_CELLS: [AtomicU64; meter::N_CELLS] = [const { AtomicU64::new(0) }; meter::N_CELLS];
static ZERO_HIST: [AtomicU64; 34] = [const { AtomicU64::new(0) }; 34];

/// The constant disarmed handles. Address of a `static` plus a literal `false`, so every test of
/// `armed` downstream folds and the instrument vanishes from the caller.
#[inline(always)]
pub fn disarmed_counter() -> Counter {
    Counter { cell: &ZERO_CELLS[0], armed: false }
}
#[inline(always)]
pub fn disarmed_meter() -> Meter {
    Meter { cells: &ZERO_CELLS, armed: false }
}
#[inline(always)]
pub fn disarmed_histogram() -> Histogram {
    Histogram { cells: &ZERO_HIST, armed: false }
}

/// A counter handle. `Copy`, so it lives in a `OnceLock` and is read without a borrow.
#[derive(Copy, Clone)]
pub struct Counter {
    cell: &'static AtomicU64,
    armed: bool,
}

impl Counter {
    #[inline]
    pub fn add(&self, n: u64) {
        if self.armed {
            self.cell.fetch_add(n, Relaxed);
        }
    }
    #[inline]
    pub fn inc(&self) {
        self.add(1);
    }
    /// For a caller that would otherwise build a value only in order to count it.
    #[inline]
    pub fn armed(&self) -> bool {
        self.armed
    }
    pub fn get(&self) -> u64 {
        self.cell.load(Relaxed)
    }
}

/// A power-of-two histogram handle.
///
/// ⭐The cells are a fixed-size ARRAY, not a slice: `record` writes three constant-ish indices, and
/// through a slice each of those carried a length load and a bounds check that the armed path paid
/// on every event. A `[AtomicU64; 34]` makes all three provably in range at compile time.
#[derive(Copy, Clone)]
pub struct Histogram {
    cells: &'static [AtomicU64; 34],
    armed: bool,
}

impl Histogram {
    /// Cells: `[0]` count, `[1]` sum, `[2..34]` buckets by `64 - v.leading_zeros()`.
    #[inline]
    pub fn record(&self, v: u64) {
        if !self.armed {
            return;
        }
        self.cells[0].fetch_add(1, Relaxed);
        self.cells[1].fetch_add(v, Relaxed);
        let b = (64 - v.leading_zeros()) as usize;
        self.cells[2 + b.min(31)].fetch_add(1, Relaxed);
    }
    #[inline]
    pub fn armed(&self) -> bool {
        self.armed
    }
}

/// Cells for ONE armed instrument, leaked so the handle can be `'static` and `Copy`. Returns an
/// ARRAY reference: every write in `Counter`, `Histogram` and `Meter` is at a compile-time-known
/// index, so an array kills the length load and the bounds check a slice made them pay per event.
fn leak_cells<const N: usize>() -> &'static [AtomicU64; N] {
    let cells: Box<[AtomicU64; N]> = Box::new(std::array::from_fn(|_| AtomicU64::new(0)));
    Box::leak(cells)
}

/// ⭐**A DISARMED INSTRUMENT DOES NOT NEED A ROW OR A CELL.** `render` filters the registry by
/// `selected` before it prints, so an entry the selector rejected was allocated, leaked, pushed
/// under a `Mutex` — and then skipped by the only code that ever reads it. With FAST DIAG on, a
/// disarmed registration hands back the shared constant handle instead and touches neither the
/// allocator nor the registry lock. Dump output is byte-identical: the rows that print are exactly
/// the rows that printed before.
#[inline]
fn skip_registration(armed: bool) -> bool {
    !armed && fast_diag()
}

/// Register a counter. Once per site, behind a `OnceLock` — see `diag_counter!`.
pub fn counter(category: &'static str, name: &'static str, help: &'static str) -> Counter {
    let armed = selected(category, name);
    if skip_registration(armed) {
        return disarmed_counter();
    }
    let cells: &'static [AtomicU64; 1] = leak_cells();
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Entry { category, name, kind: Kind::Count, cells: &cells[..], help });
    Counter { cell: &cells[0], armed }
}

/// Register a meter. Same contract as `counter`. Three cells: events, samples, cycles.
pub fn meter(category: &'static str, name: &'static str, help: &'static str) -> Meter {
    let armed = selected(category, name);
    if skip_registration(armed) {
        return disarmed_meter();
    }
    let cells: &'static [AtomicU64; meter::N_CELLS] = leak_cells();
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Entry { category, name, kind: Kind::Meter, cells: &cells[..], help });
    Meter { cells, armed }
}

/// Register a histogram. Same contract as `counter`.
pub fn histogram(category: &'static str, name: &'static str, help: &'static str) -> Histogram {
    let armed = selected(category, name);
    if skip_registration(armed) {
        return disarmed_histogram();
    }
    let cells: &'static [AtomicU64; 34] = leak_cells();
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Entry { category, name, kind: Kind::Hist, cells: &cells[..], help });
    Histogram { cells, armed }
}

/// Declare a counter at a call site.
///
/// ⭐`#[inline]` and the `none_armed` short circuit are the whole point: with nothing armed the
/// body returns a CONSTANT handle, so the caller's `armed` branch and `fetch_add` fold away and
/// what is left is one plain TLS byte test shared with every other instrument in the function.
/// Without the `#[inline]` the constant never reaches the caller and none of that can happen.
#[macro_export]
macro_rules! diag_counter {
    ($vis:vis $id:ident, $cat:expr, $name:expr, $help:expr) => {
        #[inline]
        $vis fn $id() -> $crate::diag::Counter {
            if $crate::diag::none_armed() {
                return $crate::diag::disarmed_counter();
            }
            static C: std::sync::OnceLock<$crate::diag::Counter> = std::sync::OnceLock::new();
            *C.get_or_init(|| $crate::diag::counter($cat, $name, $help))
        }
    };
}

/// Declare a METER at a call site — the instrument that answers what a population COSTS.
///
/// ```ignore
/// crate::diag_meter!(pub pic_resolve, "seam", "pic_resolve", "handle-PIC negative resolve");
/// // at the site, bound to a NAMED guard (a bare `_` would drop it at once and time nothing):
/// let _t = crate::diag::pic_resolve().start();
/// ```
///
/// A disarmed meter matters more than a disarmed counter: `start()` returns a guard with a `Drop`
/// impl, and a `Drop` that observes memory is exactly what stops the optimiser from keeping the
/// caller's state in registers across the region. With the constant handle the guard's `timing`
/// field is a literal `false`, SROA deletes the guard and the drop with it, and the metered region
/// costs the caller nothing at all.
#[macro_export]
macro_rules! diag_meter {
    ($v:vis $id:ident, $cat:expr, $name:expr, $help:expr) => {
        #[inline]
        $v fn $id() -> $crate::diag::Meter {
            if $crate::diag::none_armed() {
                return $crate::diag::disarmed_meter();
            }
            static H: std::sync::OnceLock<$crate::diag::Meter> = std::sync::OnceLock::new();
            *H.get_or_init(|| $crate::diag::meter($cat, $name, $help))
        }
    };
}

/// Declare a histogram at a call site.
#[macro_export]
macro_rules! diag_histogram {
    ($vis:vis $id:ident, $cat:expr, $name:expr, $help:expr) => {
        #[inline]
        $vis fn $id() -> $crate::diag::Histogram {
            if $crate::diag::none_armed() {
                return $crate::diag::disarmed_histogram();
            }
            static H: std::sync::OnceLock<$crate::diag::Histogram> = std::sync::OnceLock::new();
            *H.get_or_init(|| $crate::diag::histogram($cat, $name, $help))
        }
    };
}

/// A GAUGE is a value the registry READS at dump time from somewhere else. It is the bridge for
/// the ~30 instruments that already exist as bare statics in hot paths: they keep their counting
/// code and their env var, and gain a name in the registry, a help line, and every dump trigger
/// — without one edit to the path that increments them.
struct Gauge {
    category: &'static str,
    name: &'static str,
    help: &'static str,
    read: fn() -> u64,
}

fn gauges() -> &'static Mutex<Vec<Gauge>> {
    static G: OnceLock<Mutex<Vec<Gauge>>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a value the dump should read. Call from `register_builtin_gauges`, never from a hot
/// path — registration takes a lock.
pub fn gauge(category: &'static str, name: &'static str, help: &'static str, read: fn() -> u64) {
    gauges()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Gauge { category, name, help, read });
}

fn out_target() -> Option<String> {
    std::env::var("RAVA_DIAG_OUT").ok().filter(|s| s != "stderr")
}

fn json_format() -> bool {
    std::env::var("RAVA_DIAG_FORMAT").map(|v| v == "json").unwrap_or(false)
}

/// `RAVA_DIAG_DELTA=1` — print each value as the change SINCE THE PREVIOUS DUMP.
///
/// ⭐**This is what turns a flat profile into a phase map.** The engine's host profile has no
/// function above 3.4%, but its boot phases differ by 3.9x and 8.0x against a JVM, and nothing
/// could attribute a counter to a phase. With `RAVA_DIAG_EVERY_MS=2000 RAVA_DIAG_DELTA=1` every
/// instrument becomes a time series, and the phase a cost belongs to is read straight off it.
fn delta_mode() -> bool {
    std::env::var("RAVA_DIAG_DELTA").map(|v| v != "0").unwrap_or(false)
}

fn prev_values() -> &'static Mutex<std::collections::HashMap<String, u64>> {
    static P: OnceLock<Mutex<std::collections::HashMap<String, u64>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Remember `now` under `key` and hand back what was there before. Unlike `shown`, this returns
/// the PREVIOUS value rather than the difference — a meter needs three prior values to build one
/// window, and subtracts them itself.
fn shown_raw(key: &str, now: u64) -> u64 {
    let mut g = prev_values().lock().unwrap_or_else(|e| e.into_inner());
    g.insert(key.to_string(), now).unwrap_or(0)
}

/// Current value, or the change since the last dump when delta mode is on.
fn shown(key: &str, now: u64) -> u64 {
    if !delta_mode() {
        return now;
    }
    let mut g = prev_values().lock().unwrap_or_else(|e| e.into_inner());
    let before = g.insert(key.to_string(), now).unwrap_or(0);
    now.saturating_sub(before)
}

fn render(reason: &str) -> String {
    let g = registry().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<&Entry> = g.iter().filter(|e| selected(e.category, e.name)).collect();
    rows.sort_by(|a, b| (a.category, a.name).cmp(&(b.category, b.name)));
    if json_format() {
        let mut s = format!("{{\"reason\":\"{reason}\",\"entries\":[");
        for (i, e) in rows.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            match e.kind {
                Kind::Count => s.push_str(&format!(
                    "{{\"category\":\"{}\",\"name\":\"{}\",\"kind\":\"count\",\"value\":{}}}",
                    e.category,
                    e.name,
                    e.cells[0].load(Relaxed)
                )),
                Kind::Meter => s.push_str(&format!(
                    "{{\"category\":\"{}\",\"name\":\"{}\",{}}}",
                    e.category,
                    e.name,
                    meter::render_json(e.cells)
                )),
                Kind::Hist => {
                    let buckets: Vec<String> =
                        e.cells[2..].iter().map(|c| c.load(Relaxed).to_string()).collect();
                    s.push_str(&format!(
                        "{{\"category\":\"{}\",\"name\":\"{}\",\"kind\":\"hist\",\"count\":{},\"sum\":{},\"buckets\":[{}]}}",
                        e.category,
                        e.name,
                        e.cells[0].load(Relaxed),
                        e.cells[1].load(Relaxed),
                        buckets.join(",")
                    ));
                }
            }
        }
        s.push_str("]}\n");
        return s;
    }
    let gs = gauges().lock().unwrap_or_else(|e| e.into_inner());
    let mut grows: Vec<&Gauge> = gs.iter().filter(|g| selected(g.category, g.name)).collect();
    grows.sort_by(|a, b| (a.category, a.name).cmp(&(b.category, b.name)));
    let mut s = format!(
        "[DIAG] dump ({reason}{}) - {} instruments, {} gauges
",
        if delta_mode() { ", DELTA since previous" } else { "" },
        rows.len(),
        grows.len()
    );
    let mut cat = "";
    for e in rows {
        if e.category != cat {
            cat = e.category;
            s.push_str(&format!("[DIAG] == {cat} ==\n"));
        }
        match e.kind {
            Kind::Count => {
                // ⚠**AND THIS LINE READ THE TOTAL WHILE THE HEADER SAID DELTA.** Gauges went
                // through `shown` from the start; counters did not, so a `RAVA_DIAG_DELTA=1`
                // series printed gauges per-window and counters cumulative, side by side, under
                // one "DELTA since previous" banner. Read straight off, that makes a flat
                // population look like an accelerating one, which is exactly what it did to
                // `births_*` the first time this series was read.
                let key = format!("{}.{}", e.category, e.name);
                s.push_str(&format!(
                    "[DIAG]   {:<28} {:>14}   {}
",
                    e.name,
                    shown(&key, e.cells[0].load(Relaxed)),
                    e.help
                ));
            }
            Kind::Meter => {
                let body = if delta_mode() {
                    let k = format!("{}.{}", e.category, e.name);
                    let pe = shown_raw(&format!("{k}#ev"), e.cells[meter::C_EVENTS].load(Relaxed));
                    let pc = shown_raw(&format!("{k}#cy"), e.cells[meter::C_CYCLES].load(Relaxed));
                    let ps = shown_raw(&format!("{k}#smp"), e.cells[meter::C_SAMPLED].load(Relaxed));
                    meter::render_delta(e.cells, pe, pc, ps)
                } else {
                    meter::render_row(e.cells)
                };
                s.push_str(&format!(
                    "[DIAG]   {:<28} {}
[DIAG]   {:<28} {}
",
                    e.name,
                    body,
                    "",
                    e.help
                ));
            }
            Kind::Hist => {
                let n = e.cells[0].load(Relaxed);
                let sum = e.cells[1].load(Relaxed);
                let avg = if n > 0 { sum / n } else { 0 };
                s.push_str(&format!(
                    "[DIAG]   {:<28} n={:<12} avg={:<10} {}\n",
                    e.name, n, avg, e.help
                ));
                let mut line = String::new();
                for (b, c) in e.cells[2..].iter().enumerate() {
                    let v = c.load(Relaxed);
                    if v > 0 {
                        line.push_str(&format!(" 2^{b}:{v}"));
                    }
                }
                if !line.is_empty() {
                    s.push_str(&format!("[DIAG]     buckets{line}\n"));
                }
            }
        }
    }
    for g in grows {
        if g.category != cat {
            cat = g.category;
            s.push_str(&format!("[DIAG] == {cat} ==
"));
        }
        let key = format!("{}.{}", g.category, g.name);
        s.push_str(&format!(
            "[DIAG]   {:<28} {:>14}   {}
",
            g.name,
            shown(&key, (g.read)()),
            g.help
        ));
    }
    s
}

/// Write every armed instrument. Safe to call from any exit path, and from all of them: the
/// content is idempotent and `reason` says which trigger fired.
pub fn dump(reason: &str) {
    // The two REFUSAL censuses, printed wherever the arrival census is — a target picked from
    // arrivals alone is a target that may already be served.
    if crate::backedge::enabled() {
        for (tag, pfx) in [
            ("[FU-REFUSED]", "FU:"),
            ("[FU-REFUSED-ARENA]", "FUO:"),
            ("[COMBO-KEY-MISS]", "CK:"),
        ] {
            let r = crate::backedge::report_prefix(pfx, 20);
            if !r.is_empty() {
                eprintln!("{tag} {r}");
            }
        }
    }
    if crate::alloc_sites::sizes_on() {
        let r = crate::alloc_sites::size_report();
        if !r.is_empty() {
            eprint!("{r}");
        }
        let rr = crate::alloc_sites::region_report();
        if !rr.is_empty() {
            eprint!("{rr}");
        }
    }
    if selected("price", "fu_recv") {
        let r = crate::vm::fu_fastpath::fu_recv_report(16);
        if !r.is_empty() {
            eprintln!("[FU-RECV] {r}");
        }
    }
    crate::tlcache::devirt_report();
    // [DMMISS-CENSUS] — the dynamic direct-mono miss population (kind × cache word ×
    // thread), self-gated on `RAVA_DIRECTMONO_CENSUS` and counted only in the Rust miss
    // callback, so it is printed here rather than from the CLI's clean-exit dump: a
    // server boot is killed at `Done (`, and the census exists for exactly that boot.
    {
        let r = crate::directmono::report_line();
        if !r.is_empty() {
            eprintln!("{r}");
        }
    }
    if !any_armed() {
        return;
    }
    let text = render(reason);
    if let Some(path) = out_target() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(text.as_bytes());
            let _ = f.flush();
            return;
        }
    }
    eprint!("{text}");
}

/// Start the periodic dumper, the deadline dumper and the panic hook. Called once from `main`.
pub fn start_timers_if_on() {
    // Stamp the wall denominator for every meter's "% of wall" column at the START of the run,
    // not at the first meter registration.
    let _ = process_start();
    // [DMMISS-CENSUS] runs with no `RAVA_DIAG` selector at all: the census line in `dump`
    // is self-gated on `RAVA_DIRECTMONO_CENSUS`, so that env pair alone must still arm the
    // periodic timer (the self-gated census print in `dump` fires before the `any_armed`
    // early return, and no unrelated diag counter is unhidden by this). Nothing changes
    // when the census is unset — the condition is false exactly as before.
    if !any_armed() && !crate::directmono::census_on() {
        return;
    }
    if let Some(ms) =
        std::env::var("RAVA_DIAG_EVERY_MS").ok().and_then(|v| v.trim().parse::<u64>().ok())
    {
        std::thread::Builder::new()
            .name("rava-diag-timer".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                dump("timer");
            })
            .ok();
    }
    if let Some(ms) =
        std::env::var("RAVA_DIAG_EXIT_AFTER_MS").ok().and_then(|v| v.trim().parse::<u64>().ok())
    {
        std::thread::Builder::new()
            .name("rava-diag-deadline".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                dump("deadline");
                use std::io::Write;
                let _ = std::io::stderr().flush();
                std::process::exit(0);
            })
            .ok();
    }
    // A panic must not swallow the numbers that explain it.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        dump("panic");
        prev(info);
    }));
}

crate::diag_counter!(
    pub exec_compiled,
    "exec",
    "compiled",
    "guest method invocations entered as COMPILED machine code"
);
crate::diag_counter!(pub interp_has_handlers, "exec", "interp_has_handlers", "interpreted frames whose method has an exception table (never compilable today)");
crate::diag_counter!(pub interp_no_handlers, "exec", "interp_no_handlers", "interpreted frames whose method COULD be compiled");
crate::diag_counter!(
    pub exec_interpreted,
    "exec",
    "interpreted",
    "guest method invocations that built an interpreter frame instead"
);

crate::diag_counter!(
    pub mapkey_str,
    "mapkey",
    "str_fastpath",
    "map probes whose key is a String: one format! allocation and a full-length hash each"
);

crate::diag_counter!(
    pub combo_entry_hit,
    "mapkey",
    "combo_entry_hit",
    "combo-hash entries served from the per-object key-bytes memo"
);
crate::diag_counter!(
    pub combo_entry_miss,
    "mapkey",
    "combo_entry_miss",
    "combo-hash entries that had to run a full structural render"
);

crate::diag_counter!(
    pub combo_entry_prim,
    "mapkey",
    "combo_entry_prim",
    "combo-hash entries hashed in place as a primitive, no String built"
);

crate::diag_counter!(pub combo_miss_str, "mapkey", "combo_miss_str", "combo miss: Value::Str");
crate::diag_counter!(pub combo_miss_ref, "mapkey", "combo_miss_ref", "combo miss: Value::Ref (handle object)");
crate::diag_counter!(pub combo_miss_obj, "mapkey", "combo_miss_obj", "combo miss: Value::Obj the memo refused");
crate::diag_counter!(pub combo_miss_other, "mapkey", "combo_miss_other", "combo miss: any other Value kind");

crate::diag_counter!(pub combo_entry_ident, "mapkey", "combo_entry_ident", "combo entries keyed by identity instead of rendered");

crate::diag_counter!(pub iter_fastpath, "natives", "iter_fastpath", "ArrayList iterators built by cloning the backing array");

crate::diag_counter!(pub fu_site_skipped, "seam", "fu_site_skipped", "fu_fastpath attempts skipped because the site was retired");

// ⭐**THE MAP OF THE ENGINE, RE-PRICED IN SITU.** `docs/per-op-price-list.md` carries six lines,
// every price taken from an isolated `bench/NativeCost.java` case. Today an isolated estimate and
// the same site's real cost disagreed by more than 10x, so each line gets a meter and the map
// gets measured where the work actually happens.
crate::diag_histogram!(pub array_len, "price", "array_len", "length of each array at birth");
// ⭐**WHICH HALF OF A JSON PARSE IS THE COST?** The datapack layer split prices `PARSE` at
// ~380 ns per byte with the Rust tokenizer PROVEN to be serving (6,297 of 6,351 boot calls). A
// parse is two stages with completely different cures: tokenise text into a Rust AST, then
// MATERIALISE that AST as guest `JsonElement` objects. Nested-meter self-time keeps `build`'s
// allocations out of `tokenize`.
crate::diag_meter!(pub gson_build_new, "price", "gson_build_new", "new_instance per JSON tree node (by-name class+ctor resolve, then the guest constructor)");
crate::diag_meter!(pub gson_build_add, "price", "gson_build_add", "the add()/put() guest call per array element and object entry");
crate::diag_meter!(pub gson_tokenize, "price", "gson_tokenize", "Rust tokenise of a JSON document into an AST");
crate::diag_meter!(pub gson_build, "price", "gson_build", "materialising that AST as guest JsonElement objects");
crate::diag_meter!(pub native_armlookup, "price", "native_armlookup", "finding the cached arm: hashing the owner and method name");
crate::diag_meter!(pub native_lastarm, "price", "native_lastarm", "self time in the cascade's last-arm shortcut");
crate::diag_counter!(pub dalist_get_inline, "devirt", "dalist_get_inline", "DoubleArrayList.getDouble sites emitted as an inline array read");
crate::diag_counter!(pub alist_size_inline, "devirt", "alist_size_inline", "List.size() sites emitted as an inline array-length read");
crate::diag_counter!(pub devirt_emitted, "devirt", "emitted", "guarded devirtualised inlines actually emitted into machine code");
crate::diag_counter!(pub devirt_admitted, "devirt", "admitted", "monomorphic virtual/interface sites admitted for a guarded inline");
crate::diag_counter!(pub devirt_refused, "devirt", "refused", "monomorphic sites the inliner still refused (uninlinable body)");
crate::diag_counter!(pub devirt_mono_calls, "devirt", "mono_calls", "virtual/interface calls at a site that only ever saw ONE receiver class");
crate::diag_counter!(pub devirt_bi_calls, "devirt", "bi_calls", "calls at a site that saw exactly two receiver classes");
crate::diag_counter!(pub devirt_poly_calls, "devirt", "poly_calls", "calls at a site that saw three or more receiver classes");
crate::diag_counter!(pub devirt_mono_sites, "devirt", "mono_sites", "distinct call sites that only ever saw one receiver class");
crate::diag_counter!(pub devirt_bi_sites, "devirt", "bi_sites", "distinct call sites that saw exactly two");
crate::diag_counter!(pub devirt_poly_sites, "devirt", "poly_sites", "distinct call sites that saw three or more");
crate::diag_counter!(pub fu_op_get, "price", "fu_op_get", "fu_fastpath reached with name=get");
crate::diag_counter!(pub fu_op_isempty, "price", "fu_op_isempty", "fu_fastpath reached with name=isEmpty");
crate::diag_counter!(pub treenode_op, "price", "treenode_op", "gson LinkedTreeMap$Node getKey/getValue answered from one arena field");
crate::diag_counter!(pub refmap_op, "price", "refmap_op", "Reference2ObjectArrayMap get/put/containsKey answered off its packed arena arrays");
crate::diag_counter!(pub r2omap_ctor_op, "price", "r2omap_ctor_op", "Reference2ObjectArrayMap <init>(I)V answered in fu fastpath off the arena field ids");
crate::diag_counter!(pub dlist_eq, "price", "dlist_eq", "fastutil double-list equals answered off the backing arrays instead of the guest body");
crate::diag_counter!(pub fu_op_size, "price", "fu_op_size", "fu_fastpath reached with name=size");
crate::diag_counter!(pub fu_op_add, "price", "fu_op_add", "fu_fastpath reached with name=add");
crate::diag_counter!(pub fu_op_equals, "price", "fu_op_equals", "fu_fastpath reached with name=equals");
crate::diag_counter!(pub fu_op_boxval, "price", "fu_op_boxval", "fu_fastpath reached with a box unwrapper");
crate::diag_counter!(pub fu_op_hasnext, "price", "fu_op_hasnext", "fu_fastpath reached with name=hasNext");
crate::diag_counter!(pub fu_op_next, "price", "fu_op_next", "fu_fastpath reached with name=next");
crate::diag_counter!(pub fu_op_iterator, "price", "fu_op_iterator", "fu_fastpath reached with name=iterator");
crate::diag_counter!(pub fu_op_entry, "price", "fu_op_entry", "fu_fastpath reached with getKey/getValue");
crate::diag_counter!(pub fu_op_misc, "price", "fu_op_misc", "fu_fastpath reached with cast/identityHashCode");
crate::diag_counter!(pub fu_op_optional, "price", "fu_op_optional", "fu_fastpath reached with an Optional reader");
crate::diag_counter!(pub fu_op_mutate, "price", "fu_op_mutate", "fu_fastpath reached with set/remove");
crate::diag_counter!(pub fu_op_other, "price", "fu_op_other", "fu_fastpath reached with any other name");
crate::diag_counter!(pub fu_served, "price", "fu_served", "fu_fastpath answered the call from machine code");
crate::diag_counter!(pub fu_missed, "price", "fu_missed", "fu_fastpath declined after paying the attempt");
crate::diag_meter!(pub direct_probe, "price", "direct_probe", "the direct-call lookup that serves a fast crossing");
crate::diag_meter!(pub fu_fast, "price", "fu_fast", "a native served from machine code by fu_fastpath");
crate::diag_meter!(pub native_services, "price", "native_services", "the nio/net/stream/reflect/map service stages, excluding stream and map themselves");
crate::diag_meter!(pub native_stages, "price", "native_stages", "self time in the cascade's stage gauntlet, excluding the stream and map stages");
crate::diag_meter!(pub native_map, "price", "native_map", "self time in the map stage");
crate::diag_meter!(pub native_stream, "price", "native_stream", "self time in the stream/collections stage");
crate::diag_counter!(pub field_slot_hit, "price", "field_slot_hit", "field read served by the slot cache");
crate::diag_counter!(pub field_slot_miss, "price", "field_slot_miss", "field read that had to scan and refill the slot cache");
crate::diag_meter!(pub field_read, "price", "field_read", "one get_field_by_id: the linear scan a JVM does at a constant offset");
crate::diag_histogram!(pub field_scan_len, "price", "field_scan_len", "how many field ids the scan walks past");
crate::diag_counter!(pub compile_main, "price", "compile_main", "compiles that ran on the thread the boot waits for");
crate::diag_counter!(pub compile_worker, "price", "compile_worker", "compiles that ran off the critical path");
crate::diag_counter!(pub gson_native_served, "exec", "gson_native_served", "StrictJsonParser/JsonParser calls served by the Rust tokenizer");
crate::diag_counter!(pub gson_native_missed, "exec", "gson_native_missed", "the same calls that reached the intercept but it declined");
crate::diag_counter!(pub deopt_pc_known, "deopt", "pc_known", "instruction boundaries where every local's type is PROVEN (a deopt site is possible)");
crate::diag_counter!(pub deopt_pc_unknown, "deopt", "pc_unknown", "boundaries where a local's type could not be proven (deopt refused there)");
crate::diag_meter!(pub compile_self, "price", "compile_self", "self time inside the JIT compiler itself");
crate::diag_meter!(pub native_self, "price", "native_self", "self time inside a Rust native, excluding metered children");
crate::diag_meter!(pub interp_self, "price", "interp_self", "self time interpreting one frame's opcodes, excluding callees");
crate::diag_meter!(pub seam_prologue, "price", "seam_prologue", "compiled->host crossing: everything before the callee runs");
crate::diag_meter!(pub seam_prologue_full, "price", "seam_prologue_full", "crossing prologue: only what the general path adds (Arc, member_ref, args, frame)");
crate::diag_meter!(pub seam_epilogue, "price", "seam_epilogue", "compiled->host crossing: return packing after the callee");
crate::diag_meter!(pub births_array_big_price, "price", "births_array_big", "cost of an array birth of >=1024 elements");
crate::diag_meter!(pub births_array_small_price, "price", "births_array_small", "cost of an array birth of <1024 elements");
crate::diag_meter!(pub births_array_price, "price", "births_array", "cost of one array birth, in situ");
crate::diag_meter!(pub births_arena_price, "price", "births_arena", "cost of one arena object birth, in situ");
crate::diag_meter!(pub births_handle_price, "price", "births_handle", "cost of one handle-heap object birth, in situ");
crate::diag_meter!(pub mapkey_str_price, "price", "mapkey_str", "cost of rendering one string map key, in situ");
crate::diag_meter!(pub pic_resolve, "seam", "pic_resolve", "handle-PIC fill: derive the (site,class) dispatch verdict");
crate::diag_counter!(pub pic_neg_hit, "seam", "pic_neg_hit", "heap-Ref PIC fill skipped by the negative-cache probe");
crate::diag_counter!(pub iter_create_early, "natives", "iter_create_early", "iterator() served by the early cascade arm");

crate::diag_counter!(pub births_handle, "mem", "births_handle", "objects born on the handle heap");
crate::diag_counter!(pub births_array, "mem", "births_array", "arrays allocated on the heap");
crate::diag_counter!(pub births_array_ref, "mem", "births_array_ref", "of those, reference arrays — collection backings");
crate::diag_counter!(pub births_array_byte, "mem", "births_array_byte", "of those, byte arrays — IO and string bytes");
crate::diag_counter!(pub births_array_int, "mem", "births_array_int", "of those, int/char/short arrays");
crate::diag_counter!(pub births_array_long, "mem", "births_array_long", "of those, long arrays");
crate::diag_counter!(pub births_array_fp, "mem", "births_array_fp", "of those, float/double arrays");
crate::diag_counter!(pub births_arena, "mem", "births_arena", "objects born in the arena via the Rust TLAB path");

crate::diag_counter!(pub indy_concat, "indy", "concat", "invokedynamic assembled a StringConcatFactory site (\"\" + x)");
crate::diag_counter!(pub indy_lambda, "indy", "lambda", "invokedynamic assembled a LambdaMetafactory site");
crate::diag_counter!(pub indy_lambda_noncapturing, "indy", "lambda_noncapturing", "of those, capturing NOTHING — a singleton in Java, re-assembled here");
crate::diag_counter!(pub indy_record, "indy", "record", "invokedynamic assembled a record equals/hashCode/toString site");
crate::diag_counter!(pub indy_switch, "indy", "switch", "invokedynamic assembled a pattern-switch site");

/// Bridge the instruments that already exist as bare statics. Each keeps its own counting code
/// and its own env var; this gives it a NAME, a help line and every dump trigger. Called once
/// from `main`, before any guest code runs.
/// The thread the VM was constructed on — the one a boot's wall clock actually waits for.
/// `gil::is_worker()` does NOT answer this: it names GIL-free workers, and the background
/// COMPILER is neither a worker nor the main thread, so asking it counted every background
/// compile as main and made a 4.4% cost look like it was on the critical path.
pub fn main_thread_id() -> std::thread::ThreadId {
    static T: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::thread::current().id())
}

/// True on the thread the boot waits for.
pub fn on_main_thread() -> bool {
    std::thread::current().id() == main_thread_id()
}

pub fn register_builtin_gauges() {
    let _ = main_thread_id(); // stamped here: this runs on the VM thread, before any guest code
    gauge("devirt", "dg_fast", "getDouble inline arm executed", || {
        crate::jit::emit::collections::DG_FAST.load(std::sync::atomic::Ordering::Relaxed)
    });
    gauge("devirt", "dg_cls", "getDouble reached the class compare (guard 1 passed)", || {
        crate::jit::emit::collections::DG_CLS.load(std::sync::atomic::Ordering::Relaxed)
    });
    gauge("devirt", "dg_elem", "getDouble passed the class compare", || {
        crate::jit::emit::collections::DG_ELEM.load(std::sync::atomic::Ordering::Relaxed)
    });
    gauge("devirt", "dg_bnd", "getDouble found $elems to be an array", || {
        crate::jit::emit::collections::DG_BND.load(std::sync::atomic::Ordering::Relaxed)
    });
    for (n, f) in [
        ("ag_cls", crate::jit::emit::collections::AG_CLS_READ as fn() -> u64),
        ("ag_elem", crate::jit::emit::collections::AG_ELEM_READ),
        ("ag_bnd", crate::jit::emit::collections::AG_BND_READ),
        ("ag_fast", crate::jit::emit::collections::AG_FAST_READ),
    ] {
        gauge("devirt", n, "ArrayList.get inline arm: guard step reached", f);
    }
    for (n, f) in [
        ("it_cls", crate::jit::emit::iterator_inline::it_cls as fn() -> u64),
        ("it_fld", crate::jit::emit::iterator_inline::it_fld),
        ("it_fast", crate::jit::emit::iterator_inline::it_fast),
        ("nx_cls", crate::jit::emit::iterator_inline::nx_cls),
        ("nx_fast", crate::jit::emit::iterator_inline::nx_fast),
        ("cb_cls", crate::jit::emit::iterator_inline::cb_cls),
        ("cb_fast", crate::jit::emit::iterator_inline::cb_fast),
        ("cs_fast", crate::jit::emit::iterator_inline::cs_fast),
        ("ds_fast", crate::jit::emit::iterator_inline::ds_fast),
        ("db_fast", crate::jit::emit::iterator_inline::db_fast),
        ("am_sites", crate::jit::emit::collections::am_sites),
        ("am_arena", crate::jit::emit::collections::am_arena),
        ("am_fast", crate::jit::emit::collections::am_fast),
    ] {
        gauge("devirt", n, "Iterator.hasNext inline arm: guard step reached", f);
    }
    for (n, f, h) in [
        ("dalist_gate_ready", (|| crate::jit::DALIST_GATE_READY.load(R)) as fn() -> u64,
         "getDouble arm emission test reached WITH the ids published"),
        ("dalist_gate_cold", || crate::jit::DALIST_GATE_COLD.load(R),
         "…reached BEFORE they were published (compile ran first)"),
    ] {
        gauge("devirt", n, h, f);
    }
    for (n, d, f) in [
        (
            "lam_sites",
            "call sites whose closure SHAPE was recorded for the inline mint",
            crate::jit::lambda_shape::recorded as fn() -> u64,
        ),
        (
            "lam_minted",
            "closures minted from EMITTED code, no callback",
            crate::jit::lambda_shape::minted,
        ),
    ] {
        gauge("devirt", n, d, f);
    }
    for (n, d, f) in [
        (
            "tg_rec",
            "sites described as a trivial arena getter",
            crate::jit::trivial_getter::recorded as fn() -> u64,
        ),
        (
            "eg_cls",
            "Map$Entry get inline arm: guard step reached",
            crate::jit::emit::collections::eg_cls,
        ),
        (
            "eg_fast",
            "Map$Entry get inline arm: answered inline",
            crate::jit::emit::collections::eg_fast,
        ),
        (
            "mg_cls",
            "size(): an ARENA receiver reached the chained arena shape",
            crate::jit::emit::collections::mg_cls,
        ),
        (
            "mg_fast",
            "size(): a trivial arena getter answered from one cell, no crossing",
            crate::jit::emit::collections::mg_fast,
        ),
    ] {
        gauge("devirt", n, d, f);
    }
    gauge("devirt", "dg_slow", "getDouble guard missed, callback taken", || {
        crate::jit::emit::collections::DG_SLOW.load(std::sync::atomic::Ordering::Relaxed)
    });
    for (n, f, h) in [
        ("sites", crate::jit::callbacks::deopt::deopt_sites as fn() -> u64, "deopt sites emitted"),
        ("taken", crate::jit::callbacks::deopt::deopt_taken, "deopts actually executed"),
        ("thrown", crate::jit::callbacks::deopt::deopt_thrown, "deopts whose interpreted remainder threw"),
        ("invalidated", crate::jit::callbacks::deopt::deopt_invalidated, "methods dropped back to Cold so the failed speculation is recompiled away"),
    ] {
        gauge("deopt", n, h, f);
    }
    gauge("devirt", "guard_hits", "guarded inline arms actually executed (RAVA_DEVIRT_STATS)", || {
        crate::jit::emit::guarded_inline::guard_hits()
    });
    use std::sync::atomic::Ordering::Relaxed as R;
    gauge("jit", "decision_entries", "calls that reached the compile decision", || {
        crate::JIT_GATE[3].load(R) as u64
    });
    gauge("jit", "cold_bumps", "calls that bumped a method's warm-up counter", || {
        crate::JIT_GATE[4].load(R) as u64
    });
    gauge("jit", "passed_warmup", "methods that crossed the warm-up gate (events, not distinct)", || {
        crate::JIT_GATE[7].load(R) as u64
    });
    gauge("jit", "denied_deny_list", "refused by RAVA_JIT_DENY", || crate::JIT_GATE[0].load(R) as u64);
    gauge("jit", "denied_native_served", "refused: class is served natively or is a cache class", || {
        crate::JIT_GATE[1].load(R) as u64
    });
    gauge("jit", "compiled_total", "methods compiled (sync + background)", || {
        crate::COMPILED_TOTAL.load(R) as u64
    });
    gauge("jit", "compile_tried", "compile attempts on the synchronous path", || {
        crate::COMPILE_TRIED.load(R) as u64
    });
    gauge("jit", "compile_refused", "attempts the compiler itself refused", || {
        crate::COMPILE_REFUSED.load(R) as u64
    });
    gauge("jit", "queue_dropped", "warm-up passages lost because the background queue was full", || {
        crate::PLANS_NONE[3].load(R) as u64
    });
    gauge("jit", "native_served_refused", "calls refused compilation because the class is served natively (was SILENT)", || {
        crate::NATIVE_SERVED_REFUSED.load(R) as u64
    });
    gauge("jit", "queue_full", "of those, the ones lost specifically to a FULL background queue", || {
        crate::PLANS_NONE[3].load(R) as u64
    });
    gauge("jit", "plan_failed_methods", "distinct methods whose compile PLANNING failed (they retry forever)", || {
        crate::PLANS_NONE[1].load(R) as u64
    });
    gauge("jit", "cap_refused", "refused because the compiled-method cap was reached", || {
        crate::PLANS_NONE[2].load(R) as u64
    });
    for (i, lvl) in [64u32, 128, 256, 512].iter().enumerate() {
        let read: fn() -> u64 = match i {
            0 => || crate::COLD_LEVEL[0].load(R) as u64,
            1 => || crate::COLD_LEVEL[1].load(R) as u64,
            2 => || crate::COLD_LEVEL[2].load(R) as u64,
            _ => || crate::COLD_LEVEL[3].load(R) as u64,
        };
        let name: &'static str = match lvl {
            64 => "methods_over_64",
            128 => "methods_over_128",
            256 => "methods_over_256",
            _ => "methods_over_512",
        };
        gauge("jit", name, "distinct methods whose warm-up counter reached this level", read);
    }
    gauge(
        "statics",
        "getstatic_refills",
        "compiled getstatic sites that had to call the host to refill their value cell — a cold cell OR any static_gen bump",
        || crate::jit::callbacks::GETSTATIC_REFILLS.load(R),
    );
    for (n, d, f) in [
        ("offload_hits", "offload verdicts answered from the memo", crate::offload_probe::hits_n as fn() -> u64),
        ("offload_analyzed", "offload verdicts DERIVED by a transitive walk", crate::offload_probe::analyzed_n),
        ("offload_stored", "derived verdicts the epoch guard allowed to be cached", crate::offload_probe::stored_n),
        ("offload_unstable", "derived verdicts DISCARDED because a class loaded mid-derivation", crate::offload_probe::unstable_n),
    ] {
        gauge("offload", n, d, f);
    }
    gauge("statics", "writes", "putstatic executions — each builds a synthetic frame, hashes the owner name and bumps static_gen", || {
        crate::opcodes::statics::PUTSTATIC_N.load(R)
    });
    gauge("mem", "arena_allocs", "objects allocated straight into the arena", || {
        crate::nextgen::ARENA_ALLOCS.load(R)
    });
    gauge("seam", "crossings", "compiled-code calls that left for the Rust host", || {
        crate::jit_cb_census::total()
    });
    gauge("natives", "chain_calls", "native dispatches that walked a receiver's super chain", || {
        crate::natives::entry::chain_census::calls()
    });
    gauge("loader", "super_chain_calls", "super-chain cache lookups, each hashing a class NAME", || {
        crate::class_loader::SUPER_CHAIN_CALLS.load(R)
    });
    gauge("loader", "get_or_load_calls", "class lookups by name", || {
        crate::class_loader::GET_OR_LOAD_CALLS.load(R)
    });
    register_profcov_gauges();
}

/// ⭐**WHY SO FEW SITES CARRY A RECEIVER PROFILE — the funnel, printed from the PERIODIC dump.**
///
/// Selective recompilation fires ~78 times against a budget of 192, so the binding constraint is
/// profile COVERAGE, not compile budget, and nothing in the engine could say where the population
/// is lost. `sitemono::census` counts every exit of the profile's write path and every exit of
/// the emitter's read path; the six `tab_*` rows are a dump-time scan of the 65,536-slot table
/// itself. All of it arrives as GAUGES, so it prints from `RAVA_DIAG_EVERY_MS` and from every
/// other dump trigger — **not from a teardown a server boot never reaches**, which is the defect
/// five earlier censuses in this tree shipped with.
///
/// Read top to bottom:
///
///  * `w_*` — the COMPILED-CODE writer (`jit_direct_call::note_recv_profile`), one row per reason
///    it declines. `w_newrefusal` is the population it never sees at all: a (site, receiver class)
///    the direct resolver refused, so no entry was built and no receiver was ever observed.
///  * `offer_*` … `decl_filled` — what the shared write path did with the observations it got.
///    `collided` is the direct-mapped table losing a site; `decl_blind` is a heap-Ref row written
///    without a declaring class, which `mono_ref` refuses forever unless an interpreter write
///    late-fills it (`decl_filled`).
///  * `q*` — the emitter asking, on the compile path. `q_nodecl` is the one number that decides
///    the next round: sites that ARE profiled and ARE monomorphic and are thrown away only
///    because the compiled writer could not name a declaring class.
///  * `tab_*` — the standing shape of the table, independent of query counts.
fn register_profcov_gauges() {
    use crate::sitemono::census as pc;
    use std::sync::atomic::Ordering::Relaxed as R;
    for (n, d, f) in [
        ("w_fill", "direct-cache FILLs that reached the compiled-code profile writer",
            (|| pc::W_FILL.load(R)) as fn() -> u64),
        ("w_notvirt", "...declined: invokestatic/invokespecial, no receiver",
            || pc::W_NOTVIRT.load(R)),
        ("w_off", "...declined: RAVA_NO_FAST_SITEMONO", || pc::W_OFF.load(R)),
        ("w_lambda", "...declined: lambda entry shape, lambda class id, or the $Lambda name",
            || pc::W_LAMBDA.load(R)),
        ("w_guard", "...declined: entry.guard does not name this receiver's class",
            || pc::W_GUARD.load(R)),
        ("w_dead", "...declined: dead handle / unreadable class word", || pc::W_DEAD.load(R)),
        ("w_shape", "...declined: receiver is neither a heap Ref nor an arena object",
            || pc::W_SHAPE.load(R)),
        ("w_newrefusal", "NEW (site, receiver class) the resolver REFUSED - never profiled at all",
            || pc::W_NEWREFUSAL.load(R)),
        ("offer_arena", "observations offered to the table with an ARENA receiver",
            || pc::OFFER_ARENA.load(R)),
        ("offer_ref", "observations offered to the table with a HEAP-REF receiver",
            || pc::OFFER_REF.load(R)),
        ("nokey", "...refused: (caller, cp) does not pack into the 48+16-bit identity",
            || pc::NOKEY.load(R)),
        ("claimed", "...CLAIMED a free slot = distinct sites that now carry a row",
            || pc::CLAIMED.load(R)),
        ("collided", "...lost: the slot belongs to another site, so this one stays unprofiled",
            || pc::COLLIDED.load(R)),
        ("widened", "...a second receiver at a site we own: speculation correctly refused",
            || pc::WIDENED.load(R)),
        ("halfbuilt", "...silent: the owner had claimed but not yet published the row",
            || pc::HALFBUILT.load(R)),
        ("decl_blind", "heap-Ref rows claimed with NO declaring class (compiled-code-only)",
            || pc::DECL_BLIND.load(R)),
        ("decl_filled", "...of those, ones an interpreter write later completed",
            || pc::DECL_FILLED.load(R)),
        ("poison", "sites disarmed by a deopt", || pc::POISON.load(R)),
        ("poison_tomb", "...that had to claim an unowned slot as a tombstone",
            || pc::POISON_TOMB.load(R)),
        ("poison_steal", "...that had to steal a colliding site's slot",
            || pc::POISON_STEAL.load(R)),
        ("q", "emitter queries (mono_class + mono_ref), compile path only", || pc::Q.load(R)),
        ("q_epoch0", "...None: the caller-epoch proves NO site of this caller was ever profiled",
            || pc::Q_EPOCH0.load(R)),
        ("q_notable", "...None: nothing anywhere has ever been profiled",
            || pc::Q_NOTABLE.load(R)),
        ("q_foreign", "...None: this site's slot is owned by a DIFFERENT site (a real collision)",
            || pc::Q_FOREIGN.load(R)),
        ("q_empty", "...None: the slot is EMPTY - this site was simply never profiled",
            || pc::Q_EMPTY.load(R)),
        ("q_poly", "...None: the row is bimorphic, polymorphic or poisoned",
            || pc::Q_POLY.load(R)),
        ("q_kind", "...None: monomorphic, but in the other world (arena vs heap Ref)",
            || pc::Q_KIND.load(R)),
        ("q_nodecl", "...None: monomorphic heap-Ref row with NO declaring class - the lost population",
            || pc::Q_NODECL.load(R)),
        ("q_hit", "...the emitter got an answer", || pc::Q_HIT.load(R)),
    ] {
        gauge("profcov", n, d, f);
    }
    // The standing shape of the table. Each is one scan of 65,536 slots AT DUMP TIME, and an
    // unwritten table answers 0 out of a single acquire load without materialising 2 MiB.
    for (n, d, f) in [
        ("tab_rows", "slots holding a site row", pc::rows as fn() -> u64),
        ("tab_mono_arena", "…monomorphic, arena receiver — readable by the emitter", pc::mono_arena),
        ("tab_mono_ref", "…monomorphic, heap-Ref receiver WITH a declaring class — readable", pc::mono_ref),
        ("tab_mono_ref_blind", "…monomorphic, heap-Ref, NO declaring class — profiled and unreadable", pc::mono_ref_blind),
        ("tab_bi", "…bimorphic", pc::bi),
        ("tab_poly", "…polymorphic or poisoned", pc::poly),
    ] {
        gauge("profcov", n, d, f);
    }
}
