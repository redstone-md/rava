//! Arena lambdas: site interning, class ids, shaped allocation and the cell view.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use crate::arena::ArenaPtr;
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

/// Header-initialised arena-lambda in the NON-MOVING pinned region. Same layout and same
/// header writes as `tlab_alloc_lambda_shaped`; only the region differs. Never collects
/// (pure bump), so nothing can move between this call and the caller's cell writes.
pub fn pinned_alloc_lambda_shaped(shape: &ArenaLambdaShape, kind: u8) -> Option<ArenaPtr> {
    ARENA_LAMBDA_BIRTHS.fetch_add(1, Ordering::Relaxed);
    ARENA_LAMBDA_ARITY[shape.cap_kinds.len().min(8)].fetch_add(1, Ordering::Relaxed);
    let nf = (AL_HDR_CELLS + shape.cap_kinds.len()) as u32;
    let p = ArenaPtr::new(pinned_object_alloc(shape.cid, nf))?;
    unsafe {
        set_cell(p.as_ptr(), AL_KIND_SLOT, kind as i64);
        set_cell(p.as_ptr(), AL_SITE_SLOT, shape.site_idx as i64);
    }
    Some(p)
}

/// Content-interned call-site table: `u32 index ⇄ Arc<LambdaSite>`. Insert-once,
/// never removed (sites are a bounded static set — a few thousand). Deduped by
/// LambdaSite VALUE (owner/name/descriptor), so the stored index is a stable
/// identity independent of any arena address.
pub(crate) fn lambda_sites() -> &'static Mutex<(Vec<std::sync::Arc<crate::heap::LambdaSite>>, std::collections::HashMap<std::sync::Arc<crate::heap::LambdaSite>, u32>)> {
    static S: OnceLock<Mutex<(Vec<std::sync::Arc<crate::heap::LambdaSite>>, std::collections::HashMap<std::sync::Arc<crate::heap::LambdaSite>, u32>)>> = OnceLock::new();
    S.get_or_init(|| Mutex::new((Vec::new(), std::collections::HashMap::new())))
}

/// Intern a call-site → its stable table index (content-deduped).
pub fn lambda_site_intern(site: &std::sync::Arc<crate::heap::LambdaSite>) -> u32 {
    if gc_stats_enabled() {
        let t = std::time::Instant::now();
        let r = lambda_site_intern_inner(site);
        crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::LAMBDA_INTERN_NS, t.elapsed().as_nanos() as u64);
        return r;
    }
    lambda_site_intern_inner(site)
}

pub(crate) fn lambda_site_intern_inner(site: &std::sync::Arc<crate::heap::LambdaSite>) -> u32 {
    let mut g = lambda_sites().lock().unwrap();
    if let Some(&idx) = g.1.get(site) {
        return idx;
    }
    let idx = g.0.len() as u32;
    g.0.push(site.clone());
    g.1.insert(site.clone(), idx);
    idx
}

thread_local! {
    /// LOCK-FREE READ CACHE over the interned site table. Every closure SNAPSHOT
    /// (`arena_lambda_to_struct`, the per-element guest-apply path) resolves a site
    /// index, and taking the global `lambda_sites()` Mutex there cost a measured
    /// 430 ms over 10.7 M snapshots on one ChunkFill run — pure lock traffic for a
    /// table that is INSERT-ONCE and never mutates an existing entry. Because index
    /// `i` is bound to its `Arc<LambdaSite>` for the process lifetime, a per-thread
    /// copy can never go stale: it is only ever missing (then filled from the global
    /// table under the lock, once per thread per site).
    ///
    /// ⚠`UnsafeCell`, as `tlcache`'s `call`/`field` maps: the two functions that touch it do
    /// nothing but index-and-clone or append — no guest code runs while the reference is live,
    /// nothing here can re-enter this cell, and the cell is per-thread. The borrow flag was a
    /// read, a write, a branch and a drop on the closure-snapshot path.
    static SITE_READ_CACHE: UnsafeCell<Vec<std::sync::Arc<crate::heap::LambdaSite>>> =
        const { UnsafeCell::new(Vec::new()) };
}

/// Resolve an interned site index back to its shared identity. O(1) and lock-free
/// after this thread's first resolution of `idx` (see `SITE_READ_CACHE`).
pub fn lambda_site_get(idx: u32) -> Option<std::sync::Arc<crate::heap::LambdaSite>> {
    // SAFETY: see `SITE_READ_CACHE`. The reference dies inside this expression and the only
    // operations under it are a bounds-checked index and an `Arc` refcount bump.
    if let Some(hit) = SITE_READ_CACHE.with(|c| unsafe { &*c.get() }.get(idx as usize).cloned()) {
        return Some(hit);
    }
    if gc_stats_enabled() {
        let t = std::time::Instant::now();
        let r = lambda_site_get_cold(idx);
        crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::SITE_GET_NS, t.elapsed().as_nanos() as u64);
        return r;
    }
    lambda_site_get_cold(idx)
}

/// Cold path: copy the global table's new SUFFIX into this thread's read cache (entries are
/// immutable once published and index `i` is bound to its `Arc` forever, so appending what this
/// thread has not seen is exactly equivalent to a refill) and answer from it.
///
/// ⭐**THIS WAS `clear()` THEN `extend()` OVER THE WHOLE TABLE, AND THAT IS QUADRATIC.** The cold
/// path runs once per (thread, unseen site), i.e. once per site over a boot; refilling from zero
/// each time drops and re-clones every `Arc` already cached, so a table of `n` sites costs
/// `n²/2` `Arc` clone+drop pairs — two atomic read-modify-writes each — to publish `n` entries.
/// At the few thousand sites a boot links that is millions of atomics to copy pointers this
/// thread already had. The suffix append does the same work in `n` total, and cannot differ:
/// `lambda_site_intern_inner` only ever PUSHES, never replaces an index.
pub(crate) fn lambda_site_get_cold(idx: u32) -> Option<std::sync::Arc<crate::heap::LambdaSite>> {
    let g = lambda_sites().lock().unwrap();
    let hit = g.0.get(idx as usize).cloned();
    SITE_READ_CACHE.with(|c| {
        // SAFETY: see `SITE_READ_CACHE`. Nothing under this `&mut` can reach the cell again —
        // the source is the global table behind the lock we already hold, and the only work is
        // `Arc` clones.
        let c = unsafe { &mut *c.get() };
        let have = c.len();
        if g.0.len() > have {
            c.extend(g.0[have..].iter().cloned());
        }
    });
    hit
}

/// ADDRESS-pinned call sites: every `Arc<LambdaSite>` whose POINTER has been handed
/// to a dispatch cache as an identity key (`Heap::lambda_site_key`). Keyed by address,
/// NOT by content — the point is to keep that exact allocation alive for the process
/// lifetime so a cached key can never be re-minted onto a different site.
///
/// Why this is needed and `lambda_site_intern` is not enough: the resolved
/// `Plan::Lambda` that owns the canonical `Arc` lives in a THREAD-LOCAL indy cache, so
/// a worker thread's exit would drop its plans; the content interner would then decline
/// to re-insert an equal site and the freed address could be recycled. A cached entry
/// pointing at the recycled address would dispatch to the WRONG implementation with no
/// symptom — the silent-corruption class this key must not introduce. Insert-once,
/// never removed, touched only on the cold resolve path (a few thousand sites).
pub(crate) fn pinned_lambda_sites() -> &'static Mutex<std::collections::HashMap<usize, std::sync::Arc<crate::heap::LambdaSite>>> {
    static P: OnceLock<Mutex<std::collections::HashMap<usize, std::sync::Arc<crate::heap::LambdaSite>>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Pin `site`'s allocation for the process lifetime. Idempotent; cold path only.
pub fn lambda_site_pin(site: &std::sync::Arc<crate::heap::LambdaSite>) {
    let key = std::sync::Arc::as_ptr(site) as usize;
    pinned_lambda_sites().lock().unwrap().entry(key).or_insert_with(|| site.clone());
}

/// How many distinct call sites are pinned (diagnostic).
pub fn lambda_site_pinned_count() -> usize {
    pinned_lambda_sites().lock().unwrap().len()
}

/// ⭐**THE BITMAP'S BASE ADDRESS IS ONE FACT, AND IT WAS DERIVED ON EVERY MEMBERSHIP TEST.**
/// (`RAVA_NO_FAST_ARENADERIVE=1` restores the `OnceLock<Box<[AtomicU64]>>` verbatim; so does
/// `flags::emit_more_disabled()`, which zeroes the whole gate word.)
///
/// `is_lambda_cid` is the hot one: `is_arena_lambda` runs it for **every arena `Value::Obj`
/// receiver on the dispatch path** (the doc on `LAMBDA_CID_WORDS` says so and the emitted probe
/// mirrors it), and `arena_lambda_of_cell` runs it on the per-call receiver decode. Its comment
/// promised "a single atomic load". What it actually ran, per test, for a table whose address is
/// fixed for the life of the process:
///
///   1. `OnceLock::get_or_init` → `state.load(Acquire)` on the static — and an ACQUIRE load is
///      not a load LLVM may forward or hoist (EarlyCSE only caches `isUnordered()` loads; an
///      ordered one bumps the pass's generation counter and discards what it had cached), so it
///      also re-clobbers the caller's surrounding loads — the object header word included;
///   2. the `Box<[AtomicU64]>` FAT POINTER out of the `OnceLock`'s cell — a data pointer AND a
///      length, and the length is what `get(w)` then range-tests;
///   3. `*(ptr + w*8)` — the word we actually wanted, DEPENDENT on load 2, so it cannot even be
///      issued until the pointer arrives.
///
/// Three loads and a dynamic range test to reach a fixed 32 KB table. `lambda_cid_bitmap_addr()`
/// hands that very base to the EMITTER as an `iconst`, which is the proof that it is a constant:
/// the machine code has been reading it as an immediate all along while the Rust twin re-derived
/// it. IS: a `static` in BSS, so the range test is `w < 4096` against an immediate (no length to
/// load) and the read is one RIP-relative relaxed load whose address is a link-time constant.
///
/// BYTES: `LAMBDA_CID_WORDS * 8 = 32 KB of BSS`, once, for the process — the SAME 32 KB the
/// `Box` allocated on the heap, minus the allocation, the `OnceLock` and the fat pointer. Zero
/// pages until a class id touches them, and a boot registers a few thousand ids ⇒ one page.
static LAMBDA_CID_BITS: [AtomicU64; LAMBDA_CID_WORDS] =
    [const { AtomicU64::new(0) }; LAMBDA_CID_WORDS];

/// ── kill-switch arm: the pre-round storage, `OnceLock<Box<[..]>>` and all ──────────────────
fn lambda_cid_bitmap_boxed() -> &'static [AtomicU64] {
    static B: OnceLock<Box<[AtomicU64]>> = OnceLock::new();
    B.get_or_init(|| (0..LAMBDA_CID_WORDS).map(|_| AtomicU64::new(0)).collect())
}

/// The bitmap the process is using. ⚠**Both arms must agree for the WHOLE process**, because
/// `lambda_cid_bitmap_addr()` bakes this base into emitted machine code while `mark_lambda_cid`
/// writes through it: a run that marked one table and probed the other would answer "not a
/// lambda" for every closure. That holds because the selector is a bit of a `OnceLock` word
/// written once, before any class is registered and before any code is emitted, and never mutated.
pub(crate) fn lambda_cid_bitmap() -> &'static [AtomicU64] {
    if arenaderive_on(gates_word()) { &LAMBDA_CID_BITS[..] } else { lambda_cid_bitmap_boxed() }
}

/// Membership test with the gate word ALREADY IN A REGISTER — for a caller (`arena_lambda_cell`)
/// that has more than one question about the same process constant. See `classes::gates_word`.
#[inline]
fn cid_bit(g: u8, cid: u32) -> bool {
    let (w, b) = (cid as usize / 64, cid as usize % 64);
    if arenaderive_on(g) {
        // ⭐THE WHOLE TEST: a compare against an immediate and one RIP-relative relaxed load.
        return LAMBDA_CID_BITS.get(w).map_or(false, |word| word.load(Ordering::Relaxed) & (1u64 << b) != 0);
    }
    lambda_cid_bitmap_boxed().get(w).map_or(false, |word| word.load(Ordering::Relaxed) & (1u64 << b) != 0)
}

#[inline]
pub(crate) fn mark_lambda_cid(cid: u32) {
    let (w, b) = (cid as usize / 64, cid as usize % 64);
    if let Some(word) = lambda_cid_bitmap().get(w) {
        word.fetch_or(1u64 << b, Ordering::Relaxed);
    }
}
/// Is `cid` a registered arena-lambda class? Single atomic load, false by default.
#[inline]
pub fn is_lambda_cid(cid: u32) -> bool {
    cid_bit(gates_word(), cid)
}

/// Is `p` an arena-lambda object? (Its class id is in the lambda-cid bitmap.)
#[inline]
pub fn is_arena_lambda(p: ArenaPtr) -> bool {
    is_lambda_cid(unsafe { obj_class(p.as_ptr()) })
}

/// The (lazily registered) arena class-id for a capture-KIND signature. Distinct
/// signature ⇒ distinct class ⇒ distinct (correct) `ref_mask`. Cheap: a few dozen
/// distinct signatures across a whole boot (register_class dedups by name).
pub fn lambda_class_id(cap_kinds: &[u8]) -> u32 {
    lambda_class_id_for(cap_kinds, u32::MAX)
}

/// `RAVA_LAMBDA_SITE_CID` — give each impl SITE its own arena class id instead of sharing one
/// per capture signature.
///
/// ⭐**Ledger row 332.** `JIT_CALLBACK` is **25.4% of the boot** — 51.3 M machine-code exits to
/// read an answer the host already has — and the counter says why the per-site dispatch table
/// cannot absorb them: **`shift = 19,492,939` of 19,492,939 serves**, i.e. the population is
/// LAMBDAS. Two things block a lambda from that table and BOTH must go:
///
/// 1. **The key.** The table is indexed by receiver class id, and every lambda of one capture
///    signature shares one — so a hit would call another site's implementation. This flag makes
///    the id per SITE, and a site's `LambdaSite` determines the target, so the key becomes exact.
///    Sites are content-deduped into a few-thousand-entry table, which is the same order as the
///    3,009 class ids the table is already sized for.
/// 2. **The ABI**, which `dispatch_table::note_serve_direct` refuses `shift` for: the target takes
///    `raw[1..]`. The emitted PIC has served that since `emit_lambda_shift_adjust` (bit 1 of the
///    cached fptr ⇒ call with `buf+8`); the table probe decodes only bit 0.
///
/// ⚠**Cost, stated up front**: a promoted site's table is `max_class_id * 8` bytes, so doubling
/// the class-id space doubles it. A boot currently commits ~29 MB across 920 sites against the
/// 64 MiB `RAVA_DT_BUDGET_MB` ceiling, and `budget_refused` reports any refusal — so the trade is
/// visible rather than silent.
pub fn site_cid_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LAMBDA_SITE_CID").is_some())
}

/// ⛔**The site is NOT folded into the class any more — that was ledger row 333 and it cost 8×.**
/// Giving each impl site its own class multiplies the class-id space by the number of sites, and
/// per-class structures are consulted per OBJECT (`ref_mask_and_uniform` on every GC visit) and
/// per BIRTH (registry lookup, mask build, name string). The site now travels as a KEY, not as a
/// class: see `lambda_table_key`.
pub fn lambda_class_id_for(cap_kinds: &[u8], _site_idx: u32) -> u32 {
    let site = u32::MAX;
    if gc_stats_enabled() {
        let t = std::time::Instant::now();
        let r = lambda_class_id_inner_for(cap_kinds, site);
        crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::LAMBDA_CID_NS, t.elapsed().as_nanos() as u64);
        return r;
    }
    lambda_class_id_inner_for(cap_kinds, site)
}

pub(crate) fn lambda_class_id_inner(cap_kinds: &[u8]) -> u32 {
    lambda_class_id_inner_for(cap_kinds, u32::MAX)
}

pub(crate) fn lambda_class_id_inner_for(cap_kinds: &[u8], site_idx: u32) -> u32 {
    // Name encodes the capture kinds so two signatures never share a ref_mask — and, when the
    // site is known, the SITE too, so two implementations never share a dispatch-table slot.
    let mut name = String::from("[arena-lambda:");
    if site_idx != u32::MAX {
        name.push_str(&site_idx.to_string());
        name.push(':');
    }
    for (i, k) in cap_kinds.iter().enumerate() {
        if i > 0 {
            name.push('_');
        }
        name.push((b'0' + k) as char);
    }
    name.push(']');
    let name: std::sync::Arc<str> = std::sync::Arc::from(name.as_str());
    // Layout: fixed header cells (kind:int, site:int, fi:ref, cmp:ref) then one cell
    // per capture at its descriptor-derived kind. field_id = slot index (positional).
    let mut layout: Vec<(u32, u8)> = Vec::with_capacity(AL_HDR_CELLS + cap_kinds.len());
    // ⭐ONE DEFINITION of the header kinds, shared with the READER (`arena_lambda_cell`). They were
    // written here as literals and re-derived there through a full class-mirror lookup per cell;
    // naming them once is what lets the reader answer without a table AND makes drift impossible.
    layout.push((AL_KIND_SLOT as u32, AL_HDR_KINDS[AL_KIND_SLOT])); // int
    layout.push((AL_SITE_SLOT as u32, AL_HDR_KINDS[AL_SITE_SLOT])); // int
    layout.push((AL_FI_SLOT as u32, AL_HDR_KINDS[AL_FI_SLOT])); // ref
    layout.push((AL_CMP_SLOT as u32, AL_HDR_KINDS[AL_CMP_SLOT])); // ref
    for (i, &k) in cap_kinds.iter().enumerate() {
        layout.push(((AL_HDR_CELLS + i) as u32, k));
    }
    let layout: std::sync::Arc<[(u32, u8)]> = layout.into();
    let cid = register_class(&name, &layout);
    mark_lambda_cid(cid);
    cid
}

/// Bump-allocate a header-initialised arena-lambda (kind + site index set; $fi/$cmp
/// null; capture cells zeroed — the caller writes them via `set_cell` with
/// `encode_arena_cell`). `None` on a full nursery (caller collects + retries, having
/// rooted the captured values). NEVER collects itself, so any arena receiver the
/// captures point at cannot move between this call and the caller's cell writes.
pub fn tlab_alloc_lambda(cap_kinds: &[u8], site_idx: u32, kind: u8) -> Option<ArenaPtr> {
    tlab_alloc_lambda_shaped(
        &ArenaLambdaShape {
            cid: lambda_class_id_for(cap_kinds, site_idx),
            cap_kinds: cap_kinds.into(),
            site_idx,
            fi_cell: 0,
        },
        kind,
    )
}

/// The arena form of a closure, as fixed by its invokedynamic CALL SITE: the
/// capture-signature arena class id, the capture arity, and the interned impl-site
/// index. All three are functions of the site's STATIC descriptor and impl method, so
/// they are identical for every instantiation of that site — yet the birth path used
/// to re-derive all three per closure. `lambda_class_id` in particular rebuilt a class
/// NAME `String`, an `Arc<str>`, a layout `Vec` and an `Arc<[_]>`, then linear-scanned
/// the whole class registry comparing names under a global Mutex. Measured on one
/// ChunkFill run (2.59 M closures): 947 ms in `lambda_class_id` + 316 ms in
/// `lambda_site_intern`, against a total closure-minting budget of 2.79 s.
///
/// Resolving it ONCE per call site and reusing it is what `Plan::Lambda` already does
/// for the site identity itself (`tlcache::indy_put`); this is the same idea applied to
/// the representation. Nothing here is address-derived, so it carries no recycle hazard.
#[derive(Clone)]
pub struct ArenaLambdaShape {
    /// Capture-signature arena class (its `ref_mask` traces exactly the ref captures).
    pub cid: u32,
    /// Per-slot capture kinds — decides which capture cells the GC traces as refs.
    pub cap_kinds: std::sync::Arc<[u8]>,
    /// Content-interned impl-site index (a stable identity, never an address).
    pub site_idx: u32,
    /// Pre-encoded `$fi` cell (the functional-interface class name). This is a per-site
    /// CONSTANT — the indy descriptor's return type — yet it used to be re-derived and
    /// re-boxed per closure: a fresh `Arc<str>`, then `arena_box_push`, which takes a
    /// global Mutex and appends to a table that is NEVER compacted. At 2.59 M closures
    /// that is 2.59 M permanent side-table entries (≈ the +110 MiB peak working set the
    /// flag showed). Encoded once here into a form whose address is STABLE for the
    /// process — a box index (append-only ⇒ index never reused) or a PERMANENT interned
    /// arena string — so caching it is sound. A moving nursery string is deliberately
    /// not used: its address would not survive a collection.
    pub fi_cell: i64,
}

impl ArenaLambdaShape {
    /// Derive the shape for one call site. Cold path — call once per site and cache.
    /// `fi` is the functional-interface class name, or empty for none.
    pub fn resolve(
        cap_kinds: &[u8],
        impl_site: &std::sync::Arc<crate::heap::LambdaSite>,
        fi: &str,
    ) -> Self {
        let site_idx = lambda_site_intern(impl_site);
        ArenaLambdaShape {
            cid: lambda_class_id_for(cap_kinds, site_idx),
            cap_kinds: cap_kinds.into(),
            site_idx,
            fi_cell: if fi.is_empty() { 0 } else { stable_string_cell(fi) },
        }
    }
}

/// Bump-allocate a header-initialised arena-lambda from an already-resolved shape:
/// kind + site index set, `$fi`/`$cmp` null, capture cells zeroed (the caller writes
/// them via `set_cell` + `encode_arena_cell`). `None` on a full nursery — the caller
/// collects and retries, having rooted the captured values. NEVER collects itself, so
/// an arena receiver the captures point at cannot move between this call and the
/// caller's cell writes.
pub fn tlab_alloc_lambda_shaped(shape: &ArenaLambdaShape, kind: u8) -> Option<ArenaPtr> {
    // BIRTHS, not live objects. The pinned-closure experiment is admissible only if the
    // closure population is small and long-lived, and the census's 4,055 LIVE arena
    // `$Lambda` objects cannot answer that — one closure can serve many of the 2,007,617
    // applies. The pinned region never frees, so this counter is the go/no-go: a birth
    // count near the live count means pinning is safe, a birth count in the millions means
    // pinning exhausts the region and the hypothesis is dead without a build.
    ARENA_LAMBDA_BIRTHS.fetch_add(1, Ordering::Relaxed);
    ARENA_LAMBDA_ARITY[shape.cap_kinds.len().min(8)].fetch_add(1, Ordering::Relaxed);
    let nf = (AL_HDR_CELLS + shape.cap_kinds.len()) as u32;
    let p = tlab_try_alloc(shape.cid, nf)?;
    unsafe {
        set_cell(p.as_ptr(), AL_KIND_SLOT, kind as i64);
        set_cell(p.as_ptr(), AL_SITE_SLOT, shape.site_idx as i64);
    }
    Some(p)
}

/// Read an arena-lambda's MethodHandle kind.
#[inline]
pub fn arena_lambda_kind(p: ArenaPtr) -> u8 {
    unsafe { cell(p.as_ptr(), AL_KIND_SLOT) as u8 }
}
/// Read an arena-lambda's interned site index.
#[inline]
pub fn arena_lambda_site_idx(p: ArenaPtr) -> u32 {
    unsafe { cell(p.as_ptr(), AL_SITE_SLOT) as u32 }
}
/// Number of captured slots.
#[inline]
pub fn arena_lambda_cap_n(p: ArenaPtr) -> usize {
    (unsafe { obj_nfields(p.as_ptr()) } as usize).saturating_sub(AL_HDR_CELLS)
}
/// The direct-call guard for arena-lambda site `idx`.
#[inline]
pub fn arena_lambda_guard(idx: u32) -> usize {
    AL_GUARD_BASE | idx as usize
}

/// `Some((ptr, site_idx))` iff `cell` is a live arena-lambda pointer — the decode
/// every dispatch-cache arm needs before it may treat a receiver cell as a closure.
/// Mirrors the receiver-shape test the direct path already uses (aligned, inside the
/// low arena window, disjoint from the high-word Ref/Array/Box tags).
#[inline]
pub fn arena_lambda_of_cell(cell: i64) -> Option<(ArenaPtr, u32)> {
    // Flag OFF ⇒ no arena-lambda can exist ⇒ answer without touching the object
    // header. This sits on the per-call dispatch probe, so the default path must
    // not pay even one extra load for a representation it never mints.
    if !arena_lambda_enabled() {
        return None;
    }
    // `is_arena_obj_addr`, NOT the cheaper "aligned and under 2^40" shape test the
    // receiver arms use. Those arms only ever see a cell the caller already knows is
    // a receiver; this one is also asked about raw[0] of a STATIC entry, where the
    // cell is the first ARGUMENT — a small aligned int like 4096 passes the shape
    // test and then faults when the class word is read. Bounding against the live
    // arena window is what makes the header read safe rather than probable.
    if !is_arena_obj_addr(cell as u64) {
        return None;
    }
    let p = unsafe { ArenaPtr::from_addr(cell as u64) };
    if !is_arena_lambda(p) {
        return None;
    }
    // ⭐**VERIFY A CLOSURE WHERE IT IS FIRST READ, not where it was made** (`RAVA_LM_VERIFY=1`).
    //
    // The inline mint's failures all surfaced far from the mint — a closure decoded as an array,
    // a null local several calls later — because nothing checks a closure's own header against
    // the class that header names. This does: `nfields` must equal the class layout the capture
    // signature registered, and the site index must be a real interned site. A mismatch prints
    // ONCE, at the first read, with both numbers, which is the difference between a defect report
    // and another bisect.
    if lm_verify() {
        let cid = crate::nextgen::arena_class_id(p);
        let nf = unsafe { crate::nextgen::obj_nfields(p.as_ptr()) } as usize;
        let want = crate::nextgen::with_class(cid, |_, l| l.len()).unwrap_or(nf);
        let si = arena_lambda_site_idx(p) as usize;
        let nsites = lambda_sites().lock().map(|g| g.0.len()).unwrap_or(0);
        // The captures, for the one shape the inline mint fails on: five cells is small enough to
        // print and diff against a Rust-minted closure from the same site, which is the exact
        // comparison the header check cannot make.
        if nf == crate::nextgen::AL_HDR_CELLS + 5 {
            static N5: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if N5.fetch_add(1, Ordering::Relaxed) < 4 {
                let c: Vec<String> = (0..5)
                    .map(|i| format!("{:#x}", unsafe {
                        crate::nextgen::cell(p.as_ptr(), crate::nextgen::AL_HDR_CELLS + i)
                    }))
                    .collect();
                eprintln!(
                    "[LM-CAPS] cid={} site={} kind={} caps=[{}]",
                    crate::nextgen::arena_class_id(p),
                    arena_lambda_site_idx(p),
                    arena_lambda_kind(p),
                    c.join(", ")
                );
            }
        }
        if nf != want || si >= nsites {
            static ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !ONCE.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[LM-VERIFY] malformed closure: cid={cid} nfields={nf} (class layout says                      {want}) site_idx={si} of {nsites} kind={}",
                    arena_lambda_kind(p)
                );
            }
        }
    }
    Some((p, arena_lambda_site_idx(p)))
}

/// DECLARED KINDS OF THE FOUR FIXED HEADER CELLS, in slot order: `$kind` int, `$site` int,
/// `$fi` ref, `$cmp` ref. Written by `lambda_class_id_inner_for` into every arena-lambda class's
/// layout and read back by `arena_lambda_cell`, so there is exactly ONE definition and the writer
/// and the reader cannot drift.
const AL_HDR_KINDS: [u8; AL_HDR_CELLS] = [0, 0, 4, 4];

/// Raw cell + declared kind of a header/capture slot (for `decode_arena_cell`).
///
/// ⭐**THE KIND OF A LAMBDA CELL IS A CONSTANT OF THE CLASS, AND THIS DERIVED IT PER CELL — AND
/// THEN PER CELL AGAIN FOR THE NEXT CAPTURE.** (`RAVA_NO_FAST_ARENADERIVE=1` opts out; so does
/// `flags::emit_more_disabled()`.)
///
/// WAS, per call: `field_id_at(cid, slot)` → `with_class` → `with_mirror` → its OWN
/// `gates()` **acquire load**, then the `MIRROR_G` acquire load, then the `ClassMirror` record,
/// then the `layout` `Arc`'s (ptr, len), then a bounds-checked index — four dependent loads across
/// four cache lines to fetch a `(field_id, kind)` PAIR of which the caller throws the field id
/// away. That is the ONLY caller of `field_id_at` on a hot path, and it is not called once: every
/// consumer of a closure walks its captures in a loop —
///   * `opcodes::arena_lambda` (the interpreted guest-apply snapshot; the census counts
///     **2,007,617 applies through that one path** on a traced boot),
///   * `vm::lambda`, `gilfree_cf::plan` and `vm::jit_direct_invoke` (the compiled apply),
///   * `opcodes::arena_cells` for the `$fi` header cell on the closure type-test path —
/// so the whole chain runs once per CAPTURE CELL, i.e. `cap_n` times per closure read, for kinds
/// fixed when the capture signature registered its class.
///
/// IS, on the armed path: **no class-metadata lookup at all** for the four header cells (the
/// shared `AL_HDR_KINDS` constant answers), and ONE relaxed load out of the `(class, field)` L1
/// for a capture cell — the same L1 the arena field paths already keep warm for this class.
///
/// ⭐**WHY `kind_of` ANSWERS A QUESTION `field_id_at` WAS ASKED.** They index the layout
/// differently — `field_id_at` by POSITION, `kind_of` by FIELD ID — and for an arena-lambda class
/// those coincide, because `lambda_class_id_inner_for` builds the layout POSITIONALLY: it pushes
/// `(AL_KIND_SLOT, ..)`, `(AL_SITE_SLOT, ..)`, `(AL_FI_SLOT, ..)`, `(AL_CMP_SLOT, ..)` — the
/// constants 0,1,2,3 — and then `((AL_HDR_CELLS + i) as u32, cap_kinds[i])`. So the field ids of
/// such a class are exactly `0..layout.len()`, and `kind_of(cid, slot as u32)` is `field_id_at(cid,
/// slot).1` for every slot, including the out-of-range case where both answer `None` (no field id
/// ≥ `len` exists) and the caller's `unwrap_or(4)` applies unchanged.
///
/// ⚠**THE GUARD IS A PROOF, NOT A PREFILTER, AND HERE IS THE PROOF.** The fast arm is taken only
/// when `cid_bit(cid)` holds, and that bit is set by exactly one call in the process —
/// `mark_lambda_cid(cid)` at the tail of `lambda_class_id_inner_for`, on the id `register_class`
/// just returned for a `[arena-lambda:…]` name and the positional layout above. `register_class`
/// either pushes that layout or returns the id of an entry with the SAME NAME, and the name
/// encodes the capture kinds in full, so an adopted entry has an identical layout; no entry is
/// ever mutated afterwards. No other producer mints that name — it is not a representable JVM
/// class name (a `[` prefix means an array descriptor, and `arena-lambda:` is not one) — and
/// `restore_arena` only ever replays entries this same registry produced. A `cid` whose bit is
/// clear falls through to the ORIGINAL `field_id_at` line as the cold arm, so a class this
/// mechanism cannot prove anything about is served exactly as before.
///
/// The `debug_assert` re-derives the old answer and compares it on EVERY call in a debug build,
/// so the proof is checked by the differential probes rather than merely asserted here.
#[inline]
pub fn arena_lambda_cell(p: ArenaPtr, slot: usize) -> (i64, u8) {
    let cid = unsafe { obj_class(p.as_ptr()) };
    // ONE acquire load for BOTH questions this asks of the gate word — the arena-derive bit here
    // and the `GATE_LAYOUT`/`GATE_NGTLS`/`GATE_FXL1` bits inside `kind_of_g`. Reading it twice is
    // the exact cost `gates_once` was built to delete on the field paths.
    let g = gates_word();
    if arenaderive_on(g) && cid_bit(g, cid) {
        let kind = if slot < AL_HDR_CELLS {
            // The four fixed header cells: answered from the constant the layout was BUILT from.
            AL_HDR_KINDS[slot]
        } else {
            // A capture cell: one L1 probe, keyed by the field id that IS this slot index.
            kind_of_g(g, cid, slot as u32).unwrap_or(4)
        };
        debug_assert_eq!(
            kind,
            field_id_at(cid, slot).map(|(_, k)| k).unwrap_or(4),
            "arena_lambda_cell fast kind disagrees with the layout for cid={cid} slot={slot}"
        );
        return (unsafe { cell(p.as_ptr(), slot) }, kind);
    }
    // ── kill-switch / unproven-class arm: the pre-round line, verbatim ────────────────────────
    let kind = field_id_at(cid, slot).map(|(_, k)| k).unwrap_or(4);
    (unsafe { cell(p.as_ptr(), slot) }, kind)
}

// ═══════════════ Arena checkpoint/restore (snapshot roadmap, increment 4) ═══════════════

/// ONE key space shared by classes and lambda SITES.
///
/// ⭐**Ledger rows 332/333.** The per-site dispatch table is indexed by receiver class id, and
/// every lambda of one capture signature shares one — so it cannot serve a lambda without calling
/// another site's implementation. Row 333 fixed that by minting a class per site and paid **8×**
/// for it, because a class is consulted per object and per birth. **A key is not.**
///
/// Class ids live below `LAMBDA_KEY_BASE`; a lambda receiver is keyed by
/// `LAMBDA_KEY_BASE + site_idx`, read from the `AL_SITE_SLOT` cell the object already carries.
/// One class per capture shape, unchanged; the registry, the ref masks and the birth path never
/// see this.
pub const LAMBDA_KEY_BASE: u32 = 4096;

/// Stable base address of the lambda-cid bitmap, for the emitted probe's `is this a lambda` test.
/// The allocation is a `OnceLock<Box<[AtomicU64]>>` — initialised here so the address exists
/// before any code is emitted against it, and never moved afterwards.
pub fn lambda_cid_bitmap_addr() -> i64 {
    lambda_cid_bitmap().as_ptr() as i64
}

/// The dispatch-table key for an arena receiver: its class id, or `LAMBDA_KEY_BASE + site_idx`
/// when the class is a lambda class. The Rust twin of what the probe emits — both must agree, so
/// there is exactly one definition of the rule.
#[inline]
pub fn lambda_table_key(p: crate::arena::ArenaPtr, cid: u32) -> u32 {
    if !site_cid_enabled() || !is_lambda_cid(cid) {
        return cid;
    }
    let site = unsafe { cell(p.as_ptr(), AL_SITE_SLOT) } as u32;
    LAMBDA_KEY_BASE.saturating_add(site)
}

/// `RAVA_LM_VERIFY=1` — check every closure against its own class layout at first read.
fn lm_verify() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LM_VERIFY").is_some())
}
