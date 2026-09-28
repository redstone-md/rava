//! PURE-CALL MEMO — identity-keyed result cache for provably effect-free calls.
//!
//! ## Why
//! A shared DAG that is walked once per INCOMING EDGE re-derives every shared node.
//! Caching `(method, receiver, args) -> primitive result` collapses the walk back to
//! one visit per node. This is a work-REMOVING optimisation, not a body-duplicating
//! one, so it is on the right side of this tree's transfer rule.
//!
//! ## The two independent soundness problems, and how each is closed
//!
//! **(1) A pointer key is not stable.** The arena is a moving (Cheney) collector and
//! the handle table recycles slots, so a raw address/handle can be REUSED by a
//! different object — a false hit returning a silently wrong result. Closed by
//! stamping the cache with `(gc_epoch, handle_epoch)` and dropping the whole cache
//! the instant either moves. `gc_epoch` was previously bumped only by `tlab_collect`;
//! `major_collect` (which moves TENURED objects too) is now bumped as well, and a
//! handle epoch is bumped wherever an object slot is retired. Both are relaxed
//! atomic loads on the lookup path.
//!
//! **(2) Static purity is not immutability.** Proving a callee performs no store
//! proves only that THIS call does not write; it says nothing about who else writes
//! the receiver's reachable graph between two calls. The property used here instead
//! is a FROZEN EXTENT:
//!
//!   * A memo WINDOW is the dynamic extent of an outermost call whose whole subtree
//!     the existing transitive escape analysis (`Vm::is_offloadable`) accepts. That
//!     analysis rejects `putstatic`, monitors, unmodelled opcodes, non-`Pure` natives
//!     and any `putfield`/array-store whose target is not an object allocated INSIDE
//!     the call. Therefore, for the whole window, this thread cannot write any object
//!     that already existed when the window opened.
//!   * A window-born object CAN still be written by its allocator, so a key must not
//!     name one. A key reference is admitted only when it is (a) OUT of the nursery
//!     (tenured/perm — tenuring happens only at a collection, which ends the window
//!     by the epoch check, so such an object necessarily pre-dates the window), or
//!     (b) an immutable VALUE RECORD: every non-static field `final` AND primitive,
//!     so the object has no reachable graph beyond itself and the JVMS final-putfield
//!     rule confines its writes to its own `<init>`.
//!   * Writes by ANOTHER thread are excluded by requiring the GIL-exclusive state:
//!     no gil-free region open and no live guest OS thread.
//!
//!   Under those three, everything a memoized call reads is bitwise invariant for the
//!   window's duration, so replaying the first result is exact.
//!
//! **Identity, never equality.** Keys are raw operand cells: a reference cell is the
//! object's address/handle, a primitive cell its bit pattern. Distinct-but-equal
//! objects therefore MISS (conservative), and `-0.0`/`NaN` payloads compare by bits.
//!
//! **Only PRIMITIVE returns are cached.** Replaying a reference result would alias two
//! separately-allocated objects, which `==`/identity-hash can observe. A primitive
//! result has no identity, so returning the recorded bits is indistinguishable.
//!
//! **Recursion** is what the cache exists for: a nested admitted call shares the open
//! window and only the outermost frame closes it. A frame stores its result on the
//! way OUT, so a cycle can never observe a half-built entry, and an exceptional exit
//! stores nothing.



use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Operand cells one key holds: receiver + argument LOCAL SLOTS (long/double = 2).
/// 12 covers the widest shape in the ring's noise leaf, `noise(DDDDD)D` = 11 slots.
pub(crate) const MAX_CELLS: usize = 12;
/// Cap the live cache; a window that blows past this stops inserting (fail-open).
const MAX_ENTRIES: usize = 1 << 14;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("RAVA_PURE_MEMO").is_some();
        // Started HERE, in a `OnceLock` initializer that runs exactly once and is off the call
        // path. The first attempt started it from `admission::note`, which only runs in the
        // PROBE arm — so the serving arm, the one whose hit rate is the whole question, still
        // printed nothing.
        if on {
            admission::start_watchdog();
        }
        on
    })
}

/// Admission census (RAVA_PURE_MEMO_PROBE): route the same calls through the host and
/// report which the rule admits and why the rest are refused, but serve NOTHING — so
/// the census can be read without the memo changing a single result.
pub fn probe_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PURE_MEMO_PROBE").is_some())
}

/// Is the interception wired at all? Either arm needs the calls routed through the
/// host; only `enabled()` serves recorded results.
pub fn armed() -> bool {
    enabled() || probe_enabled()
}

/// What the rule makes of one resolved (class, name, desc).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Verdict {
    /// Not effect-free, or not worth intercepting — leave it on the normal fast path.
    No,
    /// Effect-free: may OPEN a memo window, but its own result is not cacheable
    /// (reference/void return, too many arguments, or too cheap to be worth a fall).
    Opener,
    /// Effect-free, primitive return, key fits — cache its result inside a window.
    Memo,
}

impl Verdict {
    /// Both non-`No` verdicts need the call routed through the host so the memo layer
    /// can see it; that is what makes the machine-code PIC refuse to cache the site.
    pub(crate) fn intercepts(self) -> bool {
        self != Verdict::No
    }
}

// ─────────────────────────── key + cache ───────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    /// `Code` pointer of the RESOLVED target — identifies the exact body, so two
    /// classes sharing an inherited implementation share entries legitimately.
    code: usize,
    n: u8,
    cells: [i64; MAX_CELLS],
}

#[derive(Default)]
struct Cache {
    map: HashMap<Key, i64>,
    /// `(gc_epoch, handle_epoch)` the entries were recorded under.
    stamp: (u64, u64),
    hits: u64,
    misses: u64,
    stores: u64,
}

impl Cache {
    /// Drop everything if either collector moved since the entries were made. A moved
    /// arena object invalidates every address key; a retired handle slot can be
    /// re-issued to an unrelated object, which would alias a `Value::Ref` key.
    fn revalidate(&mut self, now: (u64, u64)) {
        if self.stamp != now {
            use std::sync::atomic::Ordering::Relaxed;
            tally::CLEARS.fetch_add(1, Relaxed);
            tally::CLEARED_ENTRIES.fetch_add(self.map.len() as u64, Relaxed);
            self.map.clear();
            self.stamp = now;
        }
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
    /// Nesting depth of open windows; 0 = no window (nothing may be served).
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Set when something the window cannot vouch for ran; suppresses stores until
    /// the window closes.
    static TAINTED: Cell<bool> = const { Cell::new(false) };
}

fn epochs() -> (u64, u64) {
    #[cfg(feature = "next-gen-gc")]
    {
        (crate::nextgen::gc_epoch(), crate::heap::handle_epoch())
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        (0, crate::heap::handle_epoch())
    }
}

/// The GIL-exclusivity precondition: no other thread may be running guest code, so
/// nothing outside this call subtree can write the frozen graph. Two atomic loads.
pub(crate) fn exclusive() -> bool {
    !crate::parallel::any_parallel() && crate::parallel::region_count() == 0
}

// ─────────────────────────── window scope ───────────────────────────

/// Is a window open on this thread right now? Used by `taint`, and by the deferral
/// increment the admission census showed this lever still needs.
#[allow(dead_code)]
pub(crate) fn in_window() -> bool {
    DEPTH.with(|d| d.get()) > 0
}

/// Something ran that the window cannot vouch for — stop storing until it closes.
///
/// Nothing calls this yet: the window is only ever opened by a call whose WHOLE subtree
/// the effect proof accepted, so no unvouched code can run inside one. It is the hook
/// the deferred-callee increment needs (accept an unprovable virtual callee statically,
/// then taint here the instant one resolves to an unadmitted target — before it runs),
/// and it is unit-tested so that increment starts from working machinery.
#[allow(dead_code)]
pub(crate) fn taint() {
    if in_window() {
        TAINTED.with(|t| t.set(true));
        CACHE.with(|c| c.borrow_mut().map.clear());
    }
}

// ─────────────────────────── accounting ───────────────────────────

mod tally {
    use std::sync::atomic::AtomicU64;
    /// Epoch-stamp invalidations. `stores == lookups` with `hits == 0` has two very
    /// different causes -- keys that never repeat, or a cache wiped between every store and
    /// the next lookup -- and only this counter separates them.
    pub static CLEARS: AtomicU64 = AtomicU64::new(0);
    /// Clears performed by OPENING an outermost window (the dominant site, missed the first
    /// time). `WINDOW_CLEARED_ENTRIES` is what the cache HELD when each window ended.
    pub static WINDOW_CLEARS: AtomicU64 = AtomicU64::new(0);
    pub static WINDOW_CLEARED_ENTRIES: AtomicU64 = AtomicU64::new(0);
    /// Entries discarded by those invalidations: what the cache HELD when it was wiped.
    pub static CLEARED_ENTRIES: AtomicU64 = AtomicU64::new(0);
    pub static HITS: AtomicU64 = AtomicU64::new(0);
    pub static MISSES: AtomicU64 = AtomicU64::new(0);
    pub static STORES: AtomicU64 = AtomicU64::new(0);
    pub static WINDOWS: AtomicU64 = AtomicU64::new(0);
}

fn stats_roll() {
    use std::sync::atomic::Ordering::Relaxed;
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        tally::HITS.fetch_add(c.hits, Relaxed);
        tally::MISSES.fetch_add(c.misses, Relaxed);
        tally::STORES.fetch_add(c.stores, Relaxed);
        tally::WINDOWS.fetch_add(1, Relaxed);
        c.hits = 0;
        c.misses = 0;
        c.stores = 0;
    });
}

/// The descriptor's return kind: `Some(true)` = a PRIMITIVE the memo may replay,
/// `Some(false)` = void/reference (never replayed), `None` = malformed.
pub(crate) fn returns_primitive(desc: &str) -> Option<bool> {
    let after = desc.rfind(')')? + 1;
    Some(matches!(
        desc.as_bytes().get(after)?,
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z'
    ))
}

// ─────────────────── per-(site, receiver class) fast table ───────────────────

/// One resolved dispatch: the verdict, the target body's identity, and which operand
/// cells are references (so only those pay the key-stability test).
#[derive(Clone, Copy)]
pub(crate) struct Site {
    pub(crate) verdict: Verdict,
    pub(crate) code: usize,
    pub(crate) ref_mask: u8,
    pub(crate) slots: u8,
}

const SITE_WAYS: usize = 1 << 13;

struct SiteTable {
    keys: Vec<(i64, i64)>,
    vals: Vec<Option<Site>>,
}

thread_local! {
    static SITES: RefCell<SiteTable> = RefCell::new(SiteTable {
        keys: vec![(0, 0); SITE_WAYS],
        vals: vec![None; SITE_WAYS],
    });
}

#[inline]
fn site_slot(site: i64, ck: i64) -> usize {
    // Cheap mix; collisions only cost a re-resolve, never correctness (keys verified).
    let h = (site as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (ck as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    ((h >> 29) as usize) & (SITE_WAYS - 1)
}

pub(crate) fn site_get(site: i64, ck: i64) -> Option<Site> {
    SITES.with(|t| {
        let t = t.borrow();
        let k = site_slot(site, ck);
        if t.keys[k] == (site, ck) {
            t.vals[k]
        } else {
            None
        }
    })
}

pub(crate) fn site_put(site: i64, ck: i64, v: Site) {
    SITES.with(|t| {
        let mut t = t.borrow_mut();
        let k = site_slot(site, ck);
        t.keys[k] = (site, ck);
        t.vals[k] = Some(v);
    });
}

// ─────────────────── immutable value-record classes ───────────────────

/// Cache of the "value record" shape (every non-static field `final` AND primitive)
/// by class NAME. Such an object has no reachable graph beyond itself, and JVMS
/// §6.5 confines `putfield` on a `final` instance field to the declaring class's
/// `<init>` — so once constructed it can never change, whenever it was born.
static VALUE_RECORD: OnceLock<std::sync::Mutex<HashMap<String, bool>>> = OnceLock::new();

pub(crate) fn value_record_cached(name: &str) -> Option<bool> {
    VALUE_RECORD
        .get_or_init(Default::default)
        .lock()
        .ok()?
        .get(name)
        .copied()
}

pub(crate) fn value_record_put(name: &str, v: bool) {
    if let Ok(mut g) = VALUE_RECORD.get_or_init(Default::default).lock() {
        g.insert(name.to_string(), v);
    }
}

// ─────────────────────────── admission census ───────────────────────────
mod scope;
pub use scope::*;
mod tests;
pub(crate) use tests::*;
