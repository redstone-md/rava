//! Arena class registry: shapes, ref masks, layouts and field ids.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use std::cell::Cell;
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

/// Per-class shape: field count + a bit-per-cell mask of which cells are object
/// pointers (traced + forwarded by the collector).
#[derive(Clone, Copy)]
pub struct ClassShape {
    pub nfields: u32,
    pub ref_mask: u32,
}

/// One registered arena class: name + field layout `(field_id, kind)` in slot
/// order (the SAME field-ids the handle path interns, so resolution matches).
pub(crate) struct ArenaClass {
    pub(crate) name: Arc<str>,
    pub(crate) layout: Arc<[(u32, u8)]>,
    /// Dynamic reference bitmask (bit i set ⇒ cell i is a traced pointer). A word
    /// bitset (not `u32`) so a class with ANY number of fields (>32) is traced
    /// correctly — `Arc<[u64]>` is a cheap atomic clone in the GC hot loop.
    pub(crate) ref_mask: Arc<[u64]>,
    /// UNIFORM-REFS: every cell is a traced pointer, whatever the object's cell COUNT.
    ///
    /// `ref_mask` is built from `layout`, and a layout is a fixed list — which is exactly
    /// right for a class and impossible for an ARRAY, whose extent lives on the object while
    /// its class must be shared by every length. No bitmask on the class can say "all N cells
    /// are references" for varying N, at any width; a boolean can. The collectors consult
    /// this FIRST and skip the mask entirely, so an array class carries an empty layout and
    /// an empty mask and is still traced exactly.
    pub(crate) all_refs: bool,
    /// Exact guest component descriptor for a reference-array class. Immutable after CID publish.
    pub(crate) array_component: Option<Arc<str>>,
}

/// ⭐**FAST LAYOUT** (`RAVA_NO_FAST_LAYOUT=1` opts out, and the whole rewrite block opts out
/// through `flags::emit_more_disabled()` so it prices as ONE A/B).
///
/// A class's layout is frozen the instant it is registered: `register_class` either PUSHES a new
/// entry or returns the id of an existing one, and no entry is ever mutated afterwards. So the
/// slot index and the kind byte of `(class id, field id)` are CONSTANTS OF THE CLASS — deriving
/// either one per field ACCESS is repeated work by definition. Everything gated here replaces a
/// per-access (or per-allocation) scan with a structure built once per class:
///   * `slot_of` / `kind_of`: linear scan of the layout, O(fields per class) per ACCESS
///     → one open-addressed probe, O(1);
///   * `register_class` / `class_id_by_name`: linear scan of the registry with a string compare
///     per entry, O(classes registered) per ALLOCATION (the arena `new` path calls it)
///     → one hash lookup, O(1);
///   * `ref_mask_of` / `ref_mask_and_uniform` / `class_all_refs`: a GLOBAL MUTEX taken per
///     traced OBJECT in the collector → a thread-local mirror read.
///
/// The OFF arm is byte-for-byte the code that measured before this round, memo included, so the
/// A/B compares this round against the previous one and not against a straw man.
pub(crate) fn fast_layout() -> bool {
    gates() & GATE_LAYOUT != 0
}

const GATE_LAYOUT: u8 = 1; // RAVA_NO_FAST_LAYOUT — the per-class INDEX (structure)
const GATE_MIRROR: u8 = 2; // RAVA_NO_FAST_MIRROR — the pointer ACCESSOR (previous round)
const GATE_MIRROR3: u8 = 4; // RAVA_NO_FAST_MIRROR3 — the accessor's STORAGE: global, not TLS
const GATE_NGTLS: u8 = 8; // RAVA_NO_FAST_NGTLS — read the word ONCE per access, not three times
const GATE_FXL1: u8 = 16; // RAVA_NO_FAST_RESOLVETL — the (class, field) -> (slot, kind) L1
const GATE_ARENADERIVE: u8 = 32; // RAVA_NO_FAST_ARENADERIVE — the arena LAMBDA metadata reads

/// ALL THREE kill switches out of ONE `OnceLock`, because the arena field paths read them back to
/// back, on every access: `slot_of` consults `fast_layout()` and then calls `with_mirror`, which
/// consults `fast_mirror()` and `fast_mirror3()`. Three `OnceLock`s = three statics = three cache
/// lines and three acquire loads per field access; one `OnceLock<u8>` is one line, and the second
/// and third reads are the same load, which LLVM may forward because nothing opaque runs between
/// them. They stay SEPARATE BITS so the structure, its accessor and its storage still price
/// independently — sharing the storage of the switch is not sharing the switch.
///
/// ⛔**"WHICH LLVM MAY FORWARD" IS FALSE, AND THAT IS WHAT `GATE_NGTLS` FIXES.** `OnceLock::
/// get_or_init` opens with `state.load(Acquire)`, and an acquire load is not a load LLVM may
/// forward: EarlyCSE only caches loads for which `isUnordered()` holds, and an atomic load with
/// ordering **bumps the pass's generation counter**, which discards the loads it had already
/// cached. So three of them per arena field access do not merely cost three `mov`s — each one
/// re-clobbers the surrounding load cache, so the mirror pointer, the `fx` base and the layout
/// word are re-loaded around every gate test. This is the same shape `fast_birth3` priced out of
/// the birth path (five acquire loads in front of a six-instruction bump), and the fix is the
/// same: read the word ONCE at the top of the access and thread the VALUE down.
#[inline]
fn gates() -> u8 {
    static G: OnceLock<u8> = OnceLock::new();
    *G.get_or_init(|| {
        // `emit_more_disabled()` kills the whole rewrite block at once, as before.
        if crate::flags::emit_more_disabled() {
            return 0;
        }
        let mut g = 0u8;
        if std::env::var_os("RAVA_NO_FAST_LAYOUT").is_none() {
            g |= GATE_LAYOUT;
        }
        if std::env::var_os("RAVA_NO_FAST_MIRROR").is_none() {
            g |= GATE_MIRROR;
        }
        if std::env::var_os("RAVA_NO_FAST_MIRROR3").is_none() {
            g |= GATE_MIRROR3;
        }
        if std::env::var_os("RAVA_NO_FAST_NGTLS").is_none() {
            g |= GATE_NGTLS;
        }
        if std::env::var_os("RAVA_NO_FAST_RESOLVETL").is_none() {
            g |= GATE_FXL1;
        }
        // MEASURED OPT-IN (round 31) -- see `heap::fields` HG_NODERIVE for the numbers.
        if std::env::var_os("RAVA_FAST_ARENADERIVE").is_some() {
            g |= GATE_ARENADERIVE;
        }
        g
    })
}

/// THE GATE WORD ITSELF, for a caller that has more than one question and lives in another module.
///
/// `nextgen::lambdas` reads the arena-lambda metadata gate AND then asks this module for a field
/// kind; without this the two questions are two `OnceLock::get_or_init` calls, i.e. **two acquire
/// loads**, and the note on `gates()` above is precisely about why an acquire load is not free:
/// it bumps EarlyCSE's generation counter and re-clobbers every load LLVM had cached around it.
/// One fact, read once, threaded down — the same shape `gates_once` gave the field paths.
#[inline]
pub(crate) fn gates_word() -> u8 {
    gates()
}

/// Is the arena class/lambda derive block armed in `g`? (`RAVA_NO_FAST_ARENADERIVE=1` clears it,
/// and `flags::emit_more_disabled()` zeroes the whole word.) The BIT stays private so no other
/// module can spell it; the predicate is what travels.
#[inline]
pub(crate) fn arenaderive_on(g: u8) -> bool {
    g & GATE_ARENADERIVE != 0
}

/// ⭐**ONE GATE READ PER ARENA FIELD ACCESS** (`RAVA_NO_FAST_NGTLS=1` opts out; also off under
/// `emit_more_disabled()`, which zeroes the whole word).
///
/// WAS, per `getfield`/`putfield`/traced object: `fast_layout()` → acquire load #1; then
/// `with_mirror` → `fast_mirror()` acquire load #2 → `fast_mirror3()` acquire load #3. Three
/// barriers around a lookup whose ON-path body is a single RIP-relative indexed load (see
/// `with_mirror`'s FAST MIRROR 3 block) — i.e. the guard had grown larger than the thing guarded.
/// IS: one acquire load, its VALUE threaded into `with_mirror_g`, and the same three bits tested
/// as `and`/`jz` against a register.
///
/// `Some(g)` = take the threaded path; `None` = the kill switch is set, so every caller falls back
/// to the pre-round predicates verbatim and pays the reads exactly where it used to. The two arms
/// are BIT-IDENTICAL in behaviour by construction — the word is a process constant written once
/// and never mutated, so reading it once and reading it three times cannot disagree. The switch
/// exists to PRICE the barrier removal, not to guard a semantic difference.
#[inline]
fn gates_once() -> Option<u8> {
    let g = gates();
    if g & GATE_NGTLS != 0 { Some(g) } else { None }
}

/// Is cell `i` a traced reference in word-bitmask `m`?
#[inline]
pub fn ref_bit(m: &[u64], i: usize) -> bool {
    m.get(i / 64).map_or(false, |w| w & (1u64 << (i % 64)) != 0)
}
/// Does the class have NO reference cells (skip the trace loop entirely)?
#[inline]
pub fn mask_empty(m: &[u64]) -> bool {
    m.iter().all(|&w| w == 0)
}

pub(crate) fn registry() -> &'static Mutex<Vec<ArenaClass>> {
    static R: OnceLock<Mutex<Vec<ArenaClass>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// Number of registered arena classes — the upper bound a valid object header's class
/// id must be below. Read ONCE per collection so the unified mark / conservative
/// root scan can drop a false positive (a primitive that merely looks like a young
/// pointer) by its absurd class id, without locking the registry per candidate.
pub fn registry_len() -> u32 {
    registry().lock().unwrap().len() as u32
}

/// The synthetic `$…` fields rava's NATIVE implementation of `class` keeps its state in, as
/// `(name, default-kind)` — kind 0 = int, 6 = ref/null, matching `default_field_kind`.
///
/// This is a property of OUR JDK implementation, not of any jar: these fields exist because
/// rava chose to model `Optional` as a `$val`/`$present` pair rather than to run the JDK's
/// bytecode. Declaring them here is what lets `class_field_layout` give the object real arena
/// cells, exactly as `$enumName`/`$enumOrd` do for enums — after which every native reading
/// the object through get/putfield keeps working unchanged, because those paths already
/// handle both representations.
///
/// Deliberately a table and not a heuristic: a synthetic field set is something the native
/// layer KNOWS, and inferring it (say, by scanning for `$` writes) would be a guess that
/// silently under-sizes a layout — the failure mode being a putfield panic on a live object.
pub fn synthetic_layout(class: &str) -> &'static [(&'static str, u8)] {
    match class {
        // Tracer bullet: the smallest complete case (7,938 blocking cells, two fields, three
        // consumer sites). Proves the mechanism before the collections, whose sets are larger.
        "java/util/Optional" => &[("$val", 6), ("$present", 0)],
        // 9,426 blocking cells, 27% of the true residue. One field, holding an array — which
        // under the emitter's rule stops being a blocker once the object itself is arena,
        // since a synthetic holder is never bare-dereferenced.
        //
        // ⚠ DECLARED BUT NOT YET WIRED. Unlike `Optional`, whose two producers were the whole
        // surface, BitSet's natives thread a `u32` handle throughout (`recv`, `bitset_words`,
        // every op), so the layout alone changes nothing until those move to the Value-level
        // accessors. The entry is here because the synthetic SET is a fact worth recording
        // where the others live; the wiring is the next step, not a missing one.
        // ⭐**48.11% OF EVERY HANDLE BORN ON A PRODUCT BOOT** (`[PC-HANDLE]`, row 436: 6,234,501
        // handle births, and `Map$Entry` is nearly half of them). It is the ideal case for this
        // mechanism and the reason the mechanism exists: two cells, no native payload behind
        // them, and a lifetime of a few microseconds — an object that is allocated, read twice
        // and dropped, currently paying a heap handle and the LEGACY mark-sweep for the
        // privilege. `$key`/`$val` are the same two fields the handle form already used, so
        // every reader keeps working through `synth_get`.
        "java/util/Map$Entry" => &[("$key", 6), ("$val", 6)],
        "java/util/BitSet" => &[("$bits", 6)],
        _ => &[],
    }
}

/// Register a class (or return its existing id). `class_id` indexes the registry;
/// the arena object header stores it for field-slot resolution + GC ref tracing.
/// Read-only twin of `register_class`'s dedup scan: the arena class id for an already-registered
/// name, without registering anything.
///
/// ⭐Needed because an arena class id is assigned when the class is FIRST SEEN, which is after the
/// methods that call into it have compiled — and an inline arm that intercepts those calls stops
/// the native path that used to publish the id, so the publisher must not live on the path the
/// arm replaces — the arena shape chained onto `emit_cube_size_inline`.
pub fn class_id_by_name(name: &str) -> Option<u32> {
    let r = registry().lock().unwrap();
    if fast_layout() {
        return lookup_name(&r, name);
    }
    r.iter().position(|c| &*c.name == name).map(|i| i as u32)
}

/// NAME → ID INDEX for the registry's dedup scan.
///
/// WAS: `r.iter().position(|c| *c.name == **name)` — O(classes registered) with a STRING COMPARE
/// per entry, and `register_class` sits on the arena ALLOCATION path (`opcodes::alloc`'s `new`,
/// `Vm::arena_new_site`, `arena_by_name` before it memoises the route), so registering N classes
/// cost O(N²) compares and every un-memoised `new` walked the whole table. IS: one hash lookup.
///
/// ⚠**CATCH-UP, NOT INSERT-ON-PUSH.** `restore_arena` pushes entries straight onto the registry
/// without coming through here, and a map that missed those would hand out a SECOND id for a name
/// already registered — the object headers would then disagree about a class. So the index stores
/// how far it has covered and replays whatever appeared since, from the registry itself, under the
/// registry lock. That is amortised O(1) per class and correct for ANY pusher.
///
/// SIZE: one `(Arc<str>, u32)` entry per registered class (~40 B with hashbrown's load factor);
/// 4,000 arena classes ≈ 160 KB, touched only at registration — it replaces a scan that read
/// every name's bytes.
fn name_index() -> &'static Mutex<(usize, std::collections::HashMap<Arc<str>, u32>)> {
    static IX: OnceLock<Mutex<(usize, std::collections::HashMap<Arc<str>, u32>)>> = OnceLock::new();
    IX.get_or_init(|| Mutex::new((0, std::collections::HashMap::new())))
}

/// Answer `name` against `r`, catching the index up first. MUST be called with the registry lock
/// held (`r` is that guard's slice) — that is the only ordering in the process that touches both
/// locks, so no cycle exists.
fn lookup_name(r: &[ArenaClass], name: &str) -> Option<u32> {
    let mut ix = name_index().lock().unwrap_or_else(|e| e.into_inner());
    let (covered, map) = &mut *ix;
    while *covered < r.len() {
        // FIRST id wins on a duplicate name — exactly what `position` returned.
        map.entry(r[*covered].name.clone()).or_insert(*covered as u32);
        *covered += 1;
    }
    map.get(name).copied()
}

pub fn register_class(name: &Arc<str>, layout: &Arc<[(u32, u8)]>) -> u32 {
    // ⭐**FIRST-WINS IS INVISIBLE UNTIL IT BITES.** The layout handed to a SECOND call for a name
    // already present is silently discarded, so a class minted against a partial super chain keeps
    // the short record forever while every later caller believes it published the complete one.
    // `RAVA_REGCLASS_LOG=1` prints every mint and every discarded re-registration whose length
    // DIFFERS from the record — the exact event that produces "arena getfield of non-layout field".
    if std::env::var_os("RAVA_REGCLASS_LOG").is_some() {
        let existing = {
            let r = registry().lock().unwrap();
            lookup_name(&r, &**name).and_then(|id| r.get(id as usize).map(|c| c.layout.len()))
        };
        match existing {
            None => eprintln!("[REGCLASS] mint {} nf={}", name, layout.len()),
            Some(n) if n != layout.len() => eprintln!(
                "[REGCLASS] ⚠ re-register {} record_nf={} discarded_nf={}",
                name, n, layout.len()
            ),
            Some(_) => {}
        }
    }
    let mut r = registry().lock().unwrap();
    if fast_layout() {
        if let Some(id) = lookup_name(&r, &**name) {
            return id;
        }
        // Not present → fall through and push. The index catches up on the next query rather
        // than being written here, so the ONE place that learns new ids is the catch-up loop.
    } else if let Some(i) = r.iter().position(|c| *c.name == **name) {
        return i as u32;
    }
    let id = r.len() as u32;
    // ref_mask: cell i is a traced reference iff its layout kind is ref-ish
    // (kind >= 4 = the L/[ default; primitives are 0..3). The moving GC traces these.
    // Word bitset over ALL fields — no 32-field cap.
    let words = (layout.len() + 63) / 64;
    let mut ref_mask = vec![0u64; words.max(1)];
    for (i, &(_, kind)) in layout.iter().enumerate() {
        if kind >= 4 {
            ref_mask[i / 64] |= 1u64 << (i % 64);
        }
    }
    // RAVA_ARENA_PACK_PROBE (read-only telemetry): tally the field-kind distribution across
    // registered arena classes to size the Layout-Compression upside. Uniform cells are 8B each;
    // packing narrows int/float (kind 0/2) to 4B, but long/double/ref (kind 1/3/≥4) stay 8B. So the
    // savings = 4B × (int/float field count). A ref-heavy shape saves ~nothing. Structural (per-class
    // once, NOT alloc-weighted) — a first-order signal for whether the corruption-class GC/field-
    // access rewrite is worth it. Default off → zero effect.
    if std::env::var_os("RAVA_ARENA_PACK_PROBE").is_some() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CLASSES: AtomicU64 = AtomicU64::new(0);
        static NARROW: AtomicU64 = AtomicU64::new(0); // int/float fields (8B→4B packable)
        static WIDE: AtomicU64 = AtomicU64::new(0); // long/double fields (stay 8B)
        static REFS: AtomicU64 = AtomicU64::new(0); // ref fields (stay 8B)
        let (mut n, mut w, mut rf) = (0u64, 0u64, 0u64);
        for &(_, kind) in layout.iter() {
            match kind {
                0 | 2 => n += 1,
                1 | 3 => w += 1,
                _ => rf += 1,
            }
        }
        let c = CLASSES.fetch_add(1, Ordering::Relaxed) + 1;
        NARROW.fetch_add(n, Ordering::Relaxed);
        WIDE.fetch_add(w, Ordering::Relaxed);
        REFS.fetch_add(rf, Ordering::Relaxed);
        if c % 200 == 0 {
            let (nn, ww, rr) = (
                NARROW.load(Ordering::Relaxed),
                WIDE.load(Ordering::Relaxed),
                REFS.load(Ordering::Relaxed),
            );
            let total_fb = (nn + ww + rr) * 8;
            let saved = nn * 4; // each int/float 8B→4B
            let pct = if total_fb > 0 { saved * 100 / total_fb } else { 0 };
            eprintln!(
                "[PACK-PROBE] {c} arena classes: narrow(int/float)={nn} wide(long/dbl)={ww} ref={rr} \
                 | field bytes {total_fb}, packable-save {saved} ({pct}% of field region)"
            );
        }
    }
    r.push(ArenaClass {
        name: name.clone(),
        layout: layout.clone(),
        ref_mask: ref_mask.into(),
        all_refs: false,
        array_component: None,
    });
    id
}

/// Register (once) the arena class for arrays whose elements are REFERENCES. One class for
/// every such array regardless of length — the length rides `obj_nfields` on the object, and
/// `all_refs` is what lets a single class describe all of them. Primitive-element arrays need
/// no class of their own here: they are `all_refs = false` with an empty mask, which is what
/// `register_class` already produces for an empty layout.
pub fn register_uniform_ref_class(name: &str) -> u32 {
    let mut r = registry().lock().unwrap();
    if let Some(i) = r.iter().position(|c| &*c.name == name) {
        return i as u32;
    }
    let id = r.len() as u32;
    r.push(ArenaClass {
        name: Arc::from(name),
        layout: Arc::from(Vec::<(u32, u8)>::new()),
        ref_mask: Arc::from(Vec::<u64>::new()),
        all_refs: true,
        array_component: None,
    });
    id
}

/// Register an arena reference-array class with its exact component descriptor. The descriptor
/// is retained by CID, never inferred from a synthetic name, and cannot be changed by a later
/// registration of the same class descriptor.
pub(crate) fn register_ref_array_class_with_descriptor(
    descriptor: &str,
    component_descriptor: &str,
) -> u32 {
    let mut r = registry().lock().unwrap();
    let existing = if fast_layout() {
        lookup_name(&r, descriptor)
    } else {
        r.iter().position(|c| &*c.name == descriptor).map(|i| i as u32)
    };
    if let Some(id) = existing {
        let c = &r[id as usize];
        let same = c.all_refs
            && c
                .array_component
                .as_ref()
                .map(|s| &**s == component_descriptor)
                .unwrap_or(false);
        assert!(
            same,
            "arena reference-array metadata conflict for {descriptor}: existing component={:?}, requested={component_descriptor}",
            c.array_component
        );
        return id;
    }
    let id = r.len() as u32;
    r.push(ArenaClass {
        name: Arc::from(descriptor),
        layout: Arc::from(Vec::<(u32, u8)>::new()),
        ref_mask: Arc::from(Vec::<u64>::new()),
        all_refs: true,
        array_component: Some(Arc::from(component_descriptor)),
    });
    id
}

/// Legacy synthetic array classes used by the existing allocator-only scaffold. New guest
/// reference arrays should use `register_ref_array_class`, which retains the exact component
/// descriptor on the class CID. Length is not part of either identity: it rides `obj_nfields`.
pub fn arena_array_class(element_is_ref: bool) -> u32 {
    static REF_CID: OnceLock<u32> = OnceLock::new();
    static PRIM_CID: OnceLock<u32> = OnceLock::new();
    if element_is_ref {
        *REF_CID.get_or_init(|| register_uniform_ref_class("[arena-array:ref]"))
    } else {
        // Primitive elements are never traced, so this needs no uniform flag — an empty
        // layout already yields an empty mask, and `mask_empty` short-circuits the whole
        // trace loop for it. That is the correct behaviour, not an oversight.
        *PRIM_CID.get_or_init(|| {
            let name: Arc<str> = Arc::from("[arena-array:prim]");
            let empty: Arc<[(u32, u8)]> = Arc::from(Vec::<(u32, u8)>::new());
            register_class(&name, &empty)
        })
    }
}

/// Return the immutable component descriptor for an exact reference-array CID. The normal path
/// is the process-wide mirror, so readers take no registry lock after the first class touch.
pub fn ref_array_component_descriptor(class_id: u32) -> Option<Arc<str>> {
    with_mirror(class_id, |c| c.array_component.clone()).flatten()
}

/// Structural predicate for the exact descriptor-bearing reference-array class family.
#[inline]
pub fn is_ref_array_class(class_id: u32) -> bool {
    ref_array_component_descriptor(class_id).is_some()
}

/// Does every cell of `class_id` hold a traced pointer? Checked BEFORE `ref_mask_of` by both
/// collectors: a true here means "trace all `nf` cells" and makes the (empty) mask irrelevant.
pub fn class_all_refs(class_id: u32) -> bool {
    // ONE gate read for both questions this asks (see `gates_once`); `None` = kill switch, take
    // the pre-round shape, which re-reads the word inside `with_mirror`.
    if let Some(g) = gates_once() {
        if g & GATE_LAYOUT != 0 {
            return with_mirror_g(g, class_id, |c| c.all_refs).unwrap_or(false);
        }
    } else if fast_layout() {
        return with_mirror(class_id, |c| c.all_refs).unwrap_or(false);
    }
    registry().lock().unwrap().get(class_id as usize).map_or(false, |c| c.all_refs)
}

/// The empty mask handed back for an unregistered class id — a PROCESS constant, so it is built
/// once instead of allocating a fresh (header-only) `Arc` per miss.
fn empty_mask() -> Arc<[u64]> {
    static E: OnceLock<Arc<[u64]>> = OnceLock::new();
    E.get_or_init(|| Arc::from(Vec::<u64>::new())).clone()
}

/// Mask AND the uniform-refs flag in ONE registry lock. The trace loops need both on every
/// object, and taking the lock twice per object in the collector's hot path is exactly the
/// kind of cost that gets attributed to the wrong thing later.
pub fn ref_mask_and_uniform(class_id: u32) -> (Arc<[u64]>, bool) {
    // A GLOBAL MUTEX per TRACED OBJECT for two per-class constants: the lock scaled with objects
    // visited, the answer with nothing at all. Now a TLS read (the `Arc` clone stays — the
    // signature hands the mask out, and the collector's `ref_bit` loop borrows it).
    if let Some(g) = gates_once() {
        if g & GATE_LAYOUT != 0 {
            return with_mirror_g(g, class_id, |c| (c.ref_mask.clone(), c.all_refs))
                .unwrap_or_else(|| (empty_mask(), false));
        }
    } else if fast_layout() {
        return with_mirror(class_id, |c| (c.ref_mask.clone(), c.all_refs))
            .unwrap_or_else(|| (empty_mask(), false));
    }
    registry()
        .lock()
        .unwrap()
        .get(class_id as usize)
        .map(|c| (c.ref_mask.clone(), c.all_refs))
        .unwrap_or_else(|| (Arc::from(Vec::<u64>::new()), false))
}

/// Reference-cell bitmask for `class_id` (which inline cells hold object pointers).
/// `Arc` clone — cheap, no data copy; empty for an unknown class.
pub fn ref_mask_of(class_id: u32) -> Arc<[u64]> {
    if let Some(g) = gates_once() {
        if g & GATE_LAYOUT != 0 {
            return with_mirror_g(g, class_id, |c| c.ref_mask.clone()).unwrap_or_else(empty_mask);
        }
    } else if fast_layout() {
        return with_mirror(class_id, |c| c.ref_mask.clone()).unwrap_or_else(empty_mask);
    }
    registry()
        .lock()
        .unwrap()
        .get(class_id as usize)
        .map(|c| c.ref_mask.clone())
        .unwrap_or_else(|| Arc::from(Vec::<u64>::new()))
}

pub fn class_name(class_id: u32) -> Option<Arc<str>> {
    with_class(class_id, |name, _| name.clone())
}

/// LOCK-FREE READ of a registered class, through a per-thread mirror.
///
/// The registry is a `Mutex<Vec<ArenaClass>>` and every reader below took that GLOBAL MUTEX
/// to look at an immutable `(name, layout)` — `slot_of` and `kind_of` are on the arena
/// getfield/putfield paths, and the PC profile of the serial prefix puts this family at
/// **2.6%** under `field_id_at` alone. A mutex is the wrong instrument for the access
/// pattern: the registry is APPEND-ONLY (`register_class` pushes a new entry or returns the
/// index of an existing one, and no entry is ever mutated afterwards), so a class id, once
/// observed, names the same immutable record forever. That is exactly what makes a
/// per-thread mirror sound rather than merely fast.
///
/// The callback form is deliberate: returning the `Arc`s would cost two atomic increments per
/// read, which is most of what the mutex cost. On a hit this is a TLS read, a bounds check and
/// a borrow — no atomics at all. Only a miss takes the global lock, once per (thread, class).
pub(crate) fn with_class<R>(class_id: u32, f: impl FnOnce(&Arc<str>, &Arc<[(u32, u8)]>) -> R) -> Option<R> {
    with_mirror(class_id, |c| f(&c.name, &c.layout))
}

/// Empty-slot sentinel in a `FieldIndex`. A field id is an intern counter, so `u32::MAX` is
/// unreachable in practice — but the builder REFUSES to index a layout that contains it rather
/// than trusting that, and falls back to the scan (correct, merely slower).
const FX_EMPTY: u32 = u32::MAX;

/// The whole immutable record of one registered class, built ONCE per class id and shared by
/// EVERY thread through an `Arc`. Threads mirror the *pointer* (8 B per class id per thread),
/// not the contents — so the index below is one allocation per CLASS, never per thread and
/// never per object.
pub(crate) struct ClassMirror {
    pub(crate) name: Arc<str>,
    pub(crate) layout: Arc<[(u32, u8)]>,
    pub(crate) ref_mask: Arc<[u64]>,
    pub(crate) all_refs: bool,
    pub(crate) array_component: Option<Arc<str>>,
    /// FIELD INDEX — open-addressed `field_id → (slot << 8 | kind)`, capacity a power of two
    /// at load factor ≤ 0.5, so the usual lookup is ONE probe on ONE cache line. Empty when the
    /// kill switch is off, when the layout is empty, or when the builder refused it — `find`
    /// then scans the layout exactly as before.
    ///
    /// SIZE: 8 B per table slot, `next_pow2(2·nfields)` slots ⇒ **≤ 32 B per declared field**,
    /// e.g. 128 B for an 8-field class. Bounded by REGISTERED CLASSES × their field counts, not
    /// by objects: a boot registering 4,000 arena classes of ~8 fields holds ~0.5 MB in total,
    /// of which a lookup touches a single 64 B line belonging to one class. (Compare the
    /// thread-local `slot_memo` it replaces: 16 KB per thread, direct-mapped, evicting.)
    fx: Box<[(u32, u32)]>,
}

impl ClassMirror {
    /// Slot AND kind of `field_id`, in one lookup.
    ///
    /// WAS: `O(fields per class)` linear scan of `layout`, run on every arena get/putfield.
    /// IS: one multiply-shift probe into the per-class index (linear probing on collision).
    #[inline]
    pub(crate) fn find(&self, field_id: u32) -> Option<(usize, u8)> {
        if self.fx.is_empty() {
            let s = self.layout.iter().position(|&(id, _)| id == field_id)?;
            return Some((s, self.layout[s].1));
        }
        let mask = self.fx.len() - 1;
        let mut i = fx_slot(field_id, mask);
        loop {
            // In bounds by construction: `i` is masked to `fx.len()-1` on every step.
            let e = self.fx[i];
            if e.0 == field_id {
                return Some(((e.1 >> 8) as usize, (e.1 & 0xFF) as u8));
            }
            if e.0 == FX_EMPTY {
                return None;
            }
            i = (i + 1) & mask;
        }
    }
}

/// Multiply-shift: field ids are a dense intern counter, and the low bits of ids interned by
/// DIFFERENT classes overlap heavily, so the raw id would cluster. The high half of a Fibonacci
/// multiply does not.
#[inline]
fn fx_slot(field_id: u32, mask: usize) -> usize {
    ((field_id.wrapping_mul(0x9E37_79B1) >> 16) as usize) & mask
}

/// Build the field index for a layout, or an empty box meaning "scan instead".
fn build_fx(layout: &[(u32, u8)]) -> Box<[(u32, u32)]> {
    // Refusals, each of which merely costs the old scan: switch off; nothing to index; a slot
    // that will not fit the 24 packed bits; a layout carrying the empty sentinel as a field id.
    if !fast_layout() || layout.is_empty() || layout.len() > 0xFF_FFFF {
        return Box::new([]);
    }
    if layout.iter().any(|&(id, _)| id == FX_EMPTY) {
        return Box::new([]);
    }
    let cap = (layout.len() * 2).next_power_of_two().max(4);
    let mask = cap - 1;
    let mut t = vec![(FX_EMPTY, 0u32); cap];
    for (slot, &(id, kind)) in layout.iter().enumerate() {
        let mut i = fx_slot(id, mask);
        loop {
            if t[i].0 == FX_EMPTY {
                t[i] = (id, ((slot as u32) << 8) | kind as u32);
                break;
            }
            // A repeated field id keeps its FIRST slot — the same answer `position`/`find`
            // gave, so a duplicated inherited field resolves identically.
            if t[i].0 == id {
                break;
            }
            i = (i + 1) & mask;
        }
    }
    t.into_boxed_slice()
}

/// Process-wide store of built mirrors, so the SECOND thread to touch a class reuses the first
/// thread's index instead of building its own copy.
fn mirror_store() -> &'static Mutex<Vec<Option<Arc<ClassMirror>>>> {
    static S: OnceLock<Mutex<Vec<Option<Arc<ClassMirror>>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// Build (or fetch) the shared record for `class_id`. Takes the store lock and the registry lock
/// one after the other, never nested, so it adds no lock-order edge to the collector.
fn build_mirror(class_id: u32) -> Option<Arc<ClassMirror>> {
    let i = class_id as usize;
    {
        let g = mirror_store().lock().ok()?;
        if let Some(Some(c)) = g.get(i) {
            return Some(c.clone());
        }
    }
    let (name, layout, ref_mask, all_refs, array_component) = {
        let r = registry().lock().ok()?;
        let c = r.get(i)?;
        (
            c.name.clone(),
            c.layout.clone(),
            c.ref_mask.clone(),
            c.all_refs,
            c.array_component.clone(),
        )
    };
    let fx = build_fx(&layout);
    let built = Arc::new(ClassMirror { name, layout, ref_mask, all_refs, array_component, fx });
    let mut g = mirror_store().lock().ok()?;
    if g.len() <= i {
        g.resize(i + 1, None);
    }
    // A racing builder may have won; keep whoever is stored so all threads share ONE index.
    Some(g[i].get_or_insert(built).clone())
}

/// ⭐**FAST MIRROR** (`RAVA_NO_FAST_MIRROR=1` opts out; also off under `emit_more_disabled()`).
///
/// THE STRUCTURE WAS RIGHT AND THE ACCESSOR WAS NOT. The per-class index below replaced an
/// O(fields-per-class) scan and priced as a disjoint win — after which `LocalKey::with<RefCell<
/// Vec<Option<Arc<ClassMirror>>>>> … slot_of` became **1.44% of host callbacks out of compiled
/// code, plus a second `with_mirror` row at 0.63% = 2.07%** — one of the hottest callbacks in
/// the engine, for what should be one indexed load. What that accessor actually ran, per field
/// access: a `LocalKey` lazy-init/destructor-state check, a `RefCell` borrow-flag WRITE, the
/// matching check and flag restore, a `Vec` base-pointer load, a bounds check, an `Option`
/// discriminant test, and an `Arc` pointer chase to reach the record.
///
/// WAS: 8 steps + a borrow-flag store per access. IS: on a repeat of the same receiver class,
/// **one TLS load, one compare, one load** — and no store at all; on a class CHANGE, **one
/// indexed load out of a fixed TLS array**, with no `LocalKey` state byte in front of it.
///
/// THREE TIERS, in the order they are consulted:
///
///  0. **A one-entry front cache keyed on the last class id.** `slot_of`/`kind_of`/
///     `class_all_refs` are asked about the SAME receiver class over and over (a getfield site
///     is monomorphic far more often than not), so the id compare hits and no table is touched.
///
///  1. **`MIRROR_DIR`, a FIXED `[*const ClassMirror; 4096]`.** This is the tier that finishes
///     the job: class ids are a dense counter from 0, so a plain array indexed by the id is the
///     whole lookup — no `Vec` base pointer to load, no length to compare against, and, because
///     the array is `Copy` data with **no drop glue**, `thread_local!`'s const-init path emits a
///     bare `#[thread_local]` static. That means NO destructor-list registration, NO per-access
///     state byte, and no "already destroyed" panic edge. A `Vec` (or anything owning an
///     allocation) forces all three back on, which is precisely what the profile row was.
///
///  2. **`MIRROR_TAB`, the growable overflow**, for ids at or above the fixed array. It still
///     owns a `Vec`, so it is reached only when a boot registers more than 4,096 arena classes
///     AND the receiver class changes — and it is read through `try_with`, so a thread that has
///     already run TLS destructors degrades to an uncached (but still correct) lookup instead of
///     panicking.
///
/// In every tier the stored value is a raw `*const ClassMirror`, not `Option<Arc<_>>`: a null
/// pointer is the "not mirrored on this thread yet" sentinel, so the `Option` discriminant test
/// and the `Arc` deref collapse into the null check we needed anyway, and the borrow flag is
/// gone with the `RefCell`.
///
/// ⭐**WHY THE RAW POINTER CANNOT DANGLE.** `build_mirror` inserts every `Arc<ClassMirror>` into
/// `mirror_store()`, a process-lifetime `static` that is only ever APPENDED to (`resize` +
/// `get_or_insert`); no entry is ever removed, replaced or mutated, and a `static OnceLock` is
/// never dropped. So the store is the owner that keeps every mirror alive for the whole process
/// and the strong count never reaches zero. Growing that `Vec` moves the `Arc` HANDLES (one
/// pointer word each) and never the `ClassMirror` allocation they point at, so the address
/// `Arc::as_ptr` yields is stable for ever — and it stays valid past the death of the thread
/// that published it, which is what makes tier 0 answerable during TLS teardown.
/// `ClassMirror` has no interior mutability, so handing out `&ClassMirror` from that pointer
/// aliases nothing mutable. (Contrast the registry, which the mirror is built FROM: that one is
/// also append-only, which is the older half of the same argument — see `with_class`.)
///
/// ⚠**RE-ENTRANCY.** `f` may itself call `with_mirror` (a collector callback reading a second
/// class), so NO borrow of any tier — shared or unique — is ever held across `f`: each lookup
/// copies the pointer out and ends its borrow inside the `with` closure, and the miss path
/// finishes its `&mut` before calling `f`. That is the invariant the `RefCell` used to enforce
/// dynamically and the reason it could be dropped rather than merely bypassed.
///
/// BYTES, per thread: front cache 16 B (a `u32` id + an 8 B pointer, padded); `MIRROR_DIR`
/// 4,096 × 8 = **32 KB**, zero-filled from the image's TLS template at thread creation — no
/// allocation and no lazy init; `MIRROR_TAB` 8 B per class id ABOVE 4,096, and only on threads
/// that touch one. The `Option<Arc<_>>` vector this replaces was also 8 B per class id, so a
/// boot registering ~8,000 arena classes holds ~64 KB per thread either way: this is a
/// step-count change, not a memory-for-speed trade. The 32 KB is not an L1 hazard for the same
/// reason the vector was not — it is indexed directly by a dense id rather than hashed, so the
/// only lines a thread ever touches are the ones holding the classes it actually uses, and
/// tier 0 absorbs the repeats before the array is read at all.
///
/// ─────────────────────────────────────────────────────────────────────────────────────────────
/// ⭐**FAST MIRROR 3** (`RAVA_NO_FAST_MIRROR3=1` opts out; also off under `emit_more_disabled()`).
/// **THE STORAGE WAS STILL PER-THREAD, AND ON WINDOWS THAT IS NOT ONE LOAD.**
///
/// Everything above is right about the *shape* — raw pointer, no borrow flag, no `Option`, no
/// `Arc` chase — and wrong about where it lives. The prose calls tier 1 "one indexed load out of
/// a fixed TLS array" and reasons about it as `base + id*8`. On the target this engine ships for
/// (`x86_64-pc-windows-msvc`) there is no such base in a register: reaching ANY `#[thread_local]`
/// datum costs a fixed three-load dependency chain first —
///
/// ```text
///     mov  rax, [rip + _tls_index]          ; 1. which TLS slot this image owns
///     mov  rcx, gs:[0x58]                   ; 2. this thread's TLS pointer array
///     mov  rcx, [rcx + rax*8]               ; 3. this image's block inside it
///     mov  rdx, [rcx + MIRROR_DIR + id*8]   ; 4. and only now the pointer we wanted
/// ```
///
/// — four dependent loads, and tier 0 pays the same chain to save one of them, so the front
/// cache is a LOSS the moment the table it guards is not itself expensive. That is why this row
/// stayed in the profile after the storage was made drop-free: the `RefCell`, the discriminant
/// and the `Arc` were removed, and the address computation, which is most of what is left, was
/// not touched at all.
///
/// **The per-thread-ness was never a requirement.** It was inherited from `with_mirror_arc`,
/// where the mirror had to be per-thread because it held an `Arc` — cloning one is an atomic
/// increment, so a shared table would have had to hand out refcounts. A RAW pointer to an
/// immortal record has no such problem: the value is a machine word that is the SAME for every
/// thread, so the table has no business being copied per thread.
///
/// WAS: 4 dependent loads (3 of them pure address arithmetic on the TLS block) + a front cache
/// costing that same chain to elide one of them; **32 KB of TLS per thread**; and a cold miss —
/// a `mirror_store()` MUTEX — once per (thread, class), so N worker threads each rediscovered
/// every class independently.
/// IS: **one RIP-relative indexed load** out of `MIRROR_G`, a process-global static, plus the
/// null test that replaces the `Option`; **one 128 KB array for the whole process**; and the
/// cold miss taken once per CLASS, by whichever thread meets it first, for everybody.
///
/// ⭐**WHY A GLOBAL TABLE IS SOUND HERE AND NOT MERELY FASTER.** Three properties, all of them
/// already relied on above:
///  1. *The value is thread-independent.* An entry is `Arc::as_ptr` of the ONE record
///     `mirror_store()` holds for that class id — `build_mirror` resolves races through
///     `get_or_insert` under the store lock, so every thread that builds id `i` computes the
///     SAME address. Two racing publishers therefore store identical bytes and the write is
///     idempotent; no store can ever be lost in a way that matters.
///  2. *The pointee is immortal.* `mirror_store()` is a `static OnceLock` that is only appended
///     to and never dropped, so the strong count never reaches zero and the address stays valid
///     for the life of the process — the same argument the TLS tiers already make, and it is
///     what lets a pointer outlive the thread that published it. **That owner is the reason the
///     raw pointer cannot dangle**, and it is why nothing here holds an `Arc` of its own.
///  3. *The pointee is immutable and has no interior mutability*, so an unbounded number of
///     concurrent readers is a shared borrow of frozen data, not a data race.
/// `Release` on publish / `Acquire` on read is what carries the *contents* of a freshly built
/// mirror to a reader that only saw the pointer. On x86-64 both are plain `mov`s; the ordering
/// is a compiler barrier that costs nothing at run time and is not optional for correctness on
/// weaker targets.
///
/// **BYTES.** `MIRROR_G` is `MIRROR_GLOBAL × 8 = 16,384 × 8 =` **128 KB of BSS, once, for the
/// whole process** — zero pages until touched, and a boot registering ~4,000 arena classes makes
/// ~32 KB of it resident. Against what it displaces: 32 KB of TLS *per thread* (the image's TLS
/// template is copied into every thread that starts, used or not) plus 16 B of front cache per
/// thread. At eight live threads that is 256 KB against 128 KB, so this is a memory REDUCTION as
/// well as a step-count one — and the sharing is what removes the per-thread cold misses. The
/// working set is unchanged in kind: the array is indexed by a dense id, never hashed, so a
/// thread touches only the lines holding classes it actually uses (8 entries per 64 B line).
///
/// **WHAT IS DELIBERATELY NOT DONE:** no front cache in front of the global load. One indexed
/// load off a static that is Shared-clean in every core's L1 is already the floor; a cache in
/// front of it could only add a compare, and if it lived in TLS it would add the four-load chain
/// this change exists to delete. The TLS tiers below are kept verbatim as the OFF arm and as the
/// overflow path, so the A/B compares this round against the previous one and not a straw man.
#[inline]
pub(crate) fn with_mirror<R>(class_id: u32, f: impl FnOnce(&ClassMirror) -> R) -> Option<R> {
    // Callers that have no gate word of their own (`with_class` → `class_name`, `class_layout`,
    // `field_id_at`) read it here — ONE acquire load for both bits instead of the two this
    // function used to take. Callers on the field-access path pass their own; see `gates_once`.
    with_mirror_g(gates(), class_id, f)
}

/// `with_mirror` with the gate word already in a register. Split out so an arena field access
/// reads the `OnceLock` once for all three of its questions (see `gates_once`); the body below is
/// the previous round's verbatim, with `fast_mirror()`/`fast_mirror3()` replaced by tests of the
/// word those two functions each re-loaded.
#[inline]
pub(crate) fn with_mirror_g<R>(g: u8, class_id: u32, f: impl FnOnce(&ClassMirror) -> R) -> Option<R> {
    if g & GATE_MIRROR == 0 {
        return with_mirror_arc(class_id, f);
    }
    if g & GATE_MIRROR3 != 0 {
        let i = class_id as usize;
        if i < MIRROR_GLOBAL {
            // ⭐THE WHOLE LOOKUP: one RIP-relative indexed load out of a process-global array.
            // SAFETY: `i < MIRROR_GLOBAL` is the array's exact length, tested one line above.
            let p = unsafe { MIRROR_G.get_unchecked(i) }.load(Ordering::Acquire);
            if !p.is_null() {
                // SAFETY: the only pointers stored are `Arc::as_ptr` of mirrors the
                // process-lifetime `mirror_store()` owns for ever (see the dangling argument
                // above), and the `Acquire` pairs with the `Release` store in
                // `with_mirror_g_miss`, so the `ClassMirror` behind it is fully built and
                // visible. It is immutable, so this shared reference aliases nothing mutable.
                return Some(f(unsafe { &*p }));
            }
            return with_mirror_g_miss(class_id, f);
        }
        // Ids past the global array fall through to the per-thread tiers below — correct, and
        // merely as fast as the previous round. Never reached at the class counts a boot
        // registers (see `MIRROR_GLOBAL`).
    }
    // TIER 0 — FRONT CACHE. Drop-free TLS ⇒ direct access; `Cell` ⇒ no borrow flag; a hit reads
    // two words and writes nothing. The null test is NOT redundant with the id compare: the
    // const-initialised sentinel is `(u32::MAX, null)`, so a caller asking about the (never
    // registered, but representable) id `u32::MAX` must fall through rather than deref it.
    let (last_id, last_p) = MIRROR_LAST.with(|(id, p)| (id.get(), p.get()));
    if last_id == class_id && !last_p.is_null() {
        // SAFETY: only pointers published below land here, and each is `Arc::as_ptr` of a mirror
        // the process-lifetime `mirror_store()` still owns; `ClassMirror` is immutable once
        // built, so this shared reference aliases nothing mutable. The borrow of the cache ended
        // with the closure above, so `f` may re-enter freely.
        return Some(f(unsafe { &*last_p }));
    }
    let i = class_id as usize;
    if i < MIRROR_DIRECT {
        // TIER 1 — one indexed load out of a fixed, drop-free, const-initialised TLS array.
        // SAFETY: `i < MIRROR_DIRECT` is the array's exact length, tested one line above, so the
        // unchecked index is in bounds; the shared borrow of the array is confined to the
        // closure and this thread is the only one that can reach its own TLS.
        let p = MIRROR_DIR.with(|t| unsafe { *(*t.get()).get_unchecked(i) });
        if !p.is_null() {
            publish_last(class_id, p);
            // SAFETY: as tier 0 — published below, owned for ever by `mirror_store()`.
            return Some(f(unsafe { &*p }));
        }
        return with_mirror_miss(class_id, f);
    }
    // TIER 2 — overflow table for ids past the fixed array. `try_with`: this key owns a `Vec`,
    // so it IS destroyed at thread exit and `with` would panic afterwards; a late reader falls
    // through to the (correct, merely slower) build path instead.
    let p = MIRROR_TAB
        .try_with(|t| {
            // SAFETY: the shared borrow does not outlive this closure — `f` runs after it is
            // gone — and nothing inside can re-enter, so no `&mut` can exist concurrently.
            let v = unsafe { &*t.get() };
            v.get(i - MIRROR_DIRECT).copied().unwrap_or(std::ptr::null())
        })
        .unwrap_or(std::ptr::null());
    if !p.is_null() {
        publish_last(class_id, p);
        // SAFETY: as tier 0.
        return Some(f(unsafe { &*p }));
    }
    with_mirror_miss(class_id, f)
}

/// ⭐**THE MIRROR TABLE, PROCESS-GLOBAL.** `class id → *const ClassMirror`, null = not built yet.
///
/// `AtomicPtr` and not `UnsafeCell<[*const _]>` because this one IS shared: readers on every
/// thread race the single publisher of each entry. It is a plain array of words, not a map and
/// not a `Vec`, so a read is `mov reg, [rip + MIRROR_G + id*8]` with no base pointer to load
/// first — which is the entire point of moving it out of TLS (see `with_mirror`).
///
/// Const-initialised, so it lives in BSS and costs 128 KB of ADDRESS SPACE for the process, with
/// pages committed only as ids are used. No lazy init, no `OnceLock` around it, nothing to
/// destroy at exit.
static MIRROR_G: [std::sync::atomic::AtomicPtr<ClassMirror>; MIRROR_GLOBAL] =
    [const { std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()) }; MIRROR_GLOBAL];

/// Class ids answered from `MIRROR_G`. Four times the per-thread tier it replaces, because the
/// cost changed unit: this array is paid ONCE for the process rather than once per thread, so
/// headroom that was expensive as TLS is nearly free as BSS. A product boot registers on the
/// order of 4,000 arena classes (the figure the name index's own sizing note uses), so 16,384 is
/// ~4× headroom; ids beyond it are served by the per-thread tiers, i.e. exactly as fast as the
/// previous round and never slower.
///
/// 16,384 × 8 B = **128 KB of BSS, process-wide** (see the byte accounting on `with_mirror`).
const MIRROR_GLOBAL: usize = 16_384;

/// Kill switch for the STORAGE change only — global array instead of the per-thread tiers.
/// `GATE_MIRROR3` is a separate bit of the same word as `GATE_MIRROR`/`GATE_LAYOUT`, so the three
/// price independently while a field access still touches one cache line reading them.
/// `RAVA_NO_FAST_MIRROR3=1` restores the three TLS tiers verbatim, so the OFF arm is the code that
/// measured last round. (The `fast_mirror3()` accessor is gone: the bit is now tested against the
/// word `with_mirror_g` was handed, which is the whole point of `gates_once`.)
///
/// Cold half of the global path: build (or adopt) the shared record and publish its address for
/// EVERY thread. Once per class for the life of the process — not once per (thread, class), which
/// is the second thing the per-thread tiers were paying and the reason worker threads used to
/// take the `mirror_store()` mutex for classes the main thread had already mirrored.
#[cold]
#[inline(never)]
fn with_mirror_g_miss<R>(class_id: u32, f: impl FnOnce(&ClassMirror) -> R) -> Option<R> {
    // Built WITHOUT touching the table, so a reader reached from inside the build (a collector
    // callback asking about a second class) cannot meet a half-written entry.
    let arc = build_mirror(class_id)?;
    // Stable for the life of the process: `mirror_store()` holds a strong reference to this very
    // allocation and never drops it, so the local `arc` going out of scope below cannot free it.
    let p: *const ClassMirror = Arc::as_ptr(&arc);
    let i = class_id as usize;
    if i < MIRROR_GLOBAL {
        // `Release`: everything the builder wrote into the `ClassMirror` (its field index above
        // all) happens-before any `Acquire` reader that observes this pointer. A concurrent
        // publisher for the same id stores the SAME address — `build_mirror` funnels races
        // through `mirror_store()`'s `get_or_insert` — so the write is idempotent and a "lost"
        // store loses nothing.
        // SAFETY: `i < MIRROR_GLOBAL` is the array's exact length, tested one line above.
        unsafe { MIRROR_G.get_unchecked(i) }.store(p as *mut ClassMirror, Ordering::Release);
    }
    drop(arc);
    // SAFETY: `mirror_store()` still owns the allocation (the dangling argument on
    // `with_mirror`), and `ClassMirror` is immutable once built, so this shared reference
    // aliases nothing mutable.
    Some(f(unsafe { &*p }))
}

/// Number of class ids answered from the FIXED array. Class ids are handed out by
/// `register_class` as a dense counter from 0 in first-seen order, so the low ids are the
/// classes the boot touches earliest and hardest — the fixed tier covers the hot end by
/// construction, and the growable tier exists only so that a boot registering more than this
/// many arena classes stays as fast as it was before, never slower.
///
/// 4,096 × 8 B = 32 KB of per-thread TLS (see the byte accounting on `with_mirror`).
const MIRROR_DIRECT: usize = 4096;

/// Publish `p` as the last class read on this thread. Split out because all three tiers do it
/// and none of them may hold a borrow of a table while doing so.
#[inline]
fn publish_last(class_id: u32, p: *const ClassMirror) {
    MIRROR_LAST.with(|(id, c)| {
        c.set(p);
        id.set(class_id);
    });
}

thread_local! {
    /// ONE-ENTRY FRONT CACHE, last class id → its mirror. Deliberately a SEPARATE `thread_local!`
    /// from the tables: it holds only `Cell`s of `Copy` types, so it has no drop glue, so a
    /// const-initialised TLS access needs neither a lazy-init flag nor a destructor-list entry
    /// nor a per-access state byte. 16 B per thread; never destroyed, so it answers even during
    /// another key's teardown.
    static MIRROR_LAST: (Cell<u32>, Cell<*const ClassMirror>) =
        const { (Cell::new(u32::MAX), Cell::new(std::ptr::null())) };

    /// TIER 1 — class id → `*const ClassMirror` for ids < `MIRROR_DIRECT`, null = not yet
    /// mirrored on this thread. A FIXED array, not a `Vec`: `[*const T; N]` needs no drop, which
    /// is the whole point — it is what lets the const-init `thread_local!` lower to a bare
    /// `#[thread_local]` static and reduces the lookup to `base + id*8`. 32 KB per thread,
    /// zero-filled from the TLS template. `UnsafeCell`, not `RefCell`: the borrow discipline is
    /// static here (see `with_mirror`), so the flag was a per-access store buying nothing.
    static MIRROR_DIR: UnsafeCell<[*const ClassMirror; MIRROR_DIRECT]> =
        const { UnsafeCell::new([std::ptr::null(); MIRROR_DIRECT]) };

    /// TIER 2 — the same mapping for ids ≥ `MIRROR_DIRECT`, indexed by `id - MIRROR_DIRECT`.
    /// 8 B per such class id per thread, allocated only on a thread that actually meets one.
    /// This key owns a `Vec` and therefore does carry drop glue; every access to it goes through
    /// `try_with` so thread teardown degrades instead of panicking.
    static MIRROR_TAB: UnsafeCell<Vec<*const ClassMirror>> = const { UnsafeCell::new(Vec::new()) };
}

/// Kill switch for the ACCESSOR SHAPE only — the INDEX (`GATE_LAYOUT`) and the STORAGE
/// (`GATE_MIRROR3`) are separate bits of the same word, so all three price independently instead
/// of one masking another, while a field access that reads them still touches one cache line.
/// `RAVA_NO_FAST_MIRROR=1` restores `with_mirror_arc` verbatim (and therefore also disables the
/// global table, which lives behind this same shape), so the OFF arm is the pre-rewrite code.
/// (The `fast_mirror()` accessor is gone for the same reason `fast_mirror3()` is — the bit is
/// tested against the word already in a register.)
///
/// Cold half of `with_mirror`: build (or adopt) the shared record and publish its address into
/// this thread's tables. Once per (thread, class) for the life of the process.
#[cold]
#[inline(never)]
fn with_mirror_miss<R>(class_id: u32, f: impl FnOnce(&ClassMirror) -> R) -> Option<R> {
    // Built WITHOUT touching the TLS tables, so a reader reached from inside the build cannot
    // meet a half-updated one.
    let arc = build_mirror(class_id)?;
    // Stable for the life of the process: `mirror_store()` holds a strong reference to this very
    // allocation and never drops it, so the local `arc` going out of scope below cannot free it.
    let p: *const ClassMirror = Arc::as_ptr(&arc);
    let i = class_id as usize;
    if i < MIRROR_DIRECT {
        // SAFETY: `i` is in bounds by the test above; the unique borrow is confined to this
        // closure and a plain store cannot re-enter `with_mirror`, so no other borrow is live.
        MIRROR_DIR.with(|t| unsafe { *(*t.get()).get_unchecked_mut(i) = p });
    } else {
        // `try_with`: see TIER 2 above. A thread past its own teardown simply stays uncached —
        // the pointer it goes on to use is still valid, the mirrors outlive every thread.
        let _ = MIRROR_TAB.try_with(|t| {
            // SAFETY: the unique borrow is confined to this closure and nothing inside it
            // re-enters `with_mirror` (`resize`/index only), so no other borrow can be live.
            let v = unsafe { &mut *t.get() };
            let j = i - MIRROR_DIRECT;
            if v.len() <= j {
                v.resize(j + 1, std::ptr::null());
            }
            v[j] = p;
        });
    }
    publish_last(class_id, p);
    drop(arc);
    // SAFETY: `mirror_store()` still owns the allocation (argument on `with_mirror`), and
    // `ClassMirror` is immutable once built, so this shared reference aliases nothing mutable.
    Some(f(unsafe { &*p }))
}

/// ── kill-switch arm: the pre-round accessor, `RefCell` + `Arc` and all ────────────────────
///
/// LOCK-FREE READ of a registered class's whole record, through a per-thread pointer mirror.
///
/// The soundness argument is the registry's append-only-ness (see `with_class`): a class id,
/// once observed, names the same immutable record forever, so a mirrored `Arc` can never go
/// stale. On a hit this is a TLS read, a bounds check and a borrow — NO ATOMICS: `f` runs while
/// the shared borrow is held precisely so the `Arc` need not be cloned. Only a miss takes a
/// lock, once per (thread, class).
fn with_mirror_arc<R>(class_id: u32, f: impl FnOnce(&ClassMirror) -> R) -> Option<R> {
    thread_local! {
        // 8 B per class id per thread (niche-optimised `Option<Arc<_>>`), against 16 B for the
        // (name, layout) pair this replaces — and the pointed-to record is shared, not copied.
        static MIRROR: RefCell<Vec<Option<Arc<ClassMirror>>>> = const { RefCell::new(Vec::new()) };
    }
    MIRROR.with(|m| {
        let i = class_id as usize;
        {
            let v = m.borrow();
            if let Some(Some(c)) = v.get(i) {
                return Some(f(c));
            }
        }
        // Miss: built WITHOUT holding the mirror borrow, so a reader inside the build cannot
        // deadlock against this thread's own TLS.
        let built = build_mirror(class_id)?;
        let c = {
            let mut v = m.borrow_mut();
            if v.len() <= i {
                v.resize(i + 1, None);
            }
            v[i] = Some(built);
            v[i].clone().expect("just inserted")
        };
        // Borrow released before `f`: one refcount pair, once per (thread, class), buys the
        // guarantee that a re-entrant reader inside `f` sees an unborrowed mirror.
        Some(f(&c))
    })
}

/// M5.1: the class's (name, field-layout) — layout = (field_id, kind) per cell. The kind byte encodes
/// the field type (the structkey scan summarises these to size a future 64-bit packer). Clones out.
pub fn class_layout(class_id: u32) -> Option<(Arc<str>, Arc<[(u32, u8)]>)> {
    with_class(class_id, |n, l| (n.clone(), l.clone()))
}

// ═══ THE ANSWER IN ONE LOAD — a (class, field) -> (slot, kind) L1 ═══════════════════════════
//
// ⭐**`RAVA_NO_FAST_RESOLVETL=1` opts out** (and so does `flags::emit_more_disabled()`, which
// zeroes the whole gate word), restoring the previous round's path verbatim.
//
// WHAT WAS LEFT. The per-class field INDEX was the right structure — it turned an
// O(fields-per-class) scan into one probe — and the accessor rewrite that followed it moved the
// mirror pointer out of TLS into a process-global array, so reaching a class is now one
// RIP-relative load. `nextgen::classes::slot_of` is nevertheless still **1.21% of host callback
// samples**, and the reason is that "one probe" is spread over THREE DEPENDENT LOADS ON THREE
// DIFFERENT CACHE LINES, none of which can start before the previous one retires:
//
//   1. `MIRROR_G[class_id]`   — a line of the 128 KB global mirror table (an acquire load);
//   2. `mirror.fx` (ptr, len) — a line of that class's `ClassMirror` record;
//   3. `fx[fx_slot(field_id)]`— a line of that class's field-index table.
//
// The QUESTION, though, has a two-word answer that is a constant of the pair: the slot index and
// the kind byte. So the whole chain is a lookup of something that fits, key and all, in ONE
// 64-bit word — which is what this table stores.
//
// WAS: three dependent loads across three lines, per arena `getfield`/`putfield` (interpreted, via
// `opcodes::arena_cells`) and per JIT field callback (`jit::callbacks::fields`).
// IS: one relaxed load out of a small direct-mapped array, a 40-bit compare, and two shifts.
//
// ⭐**WHY IT IS PROCESS-GLOBAL AND NOT PER-THREAD**, which is the same argument `MIRROR_G` makes
// one level down: the value is thread-independent (a layout is registered once and never mutated
// — `register_class` pushes or returns an existing id, `restore_arena` only ever appends, and no
// entry is replaced), so a per-thread copy would buy nothing and would cost the Windows TLS
// three-load address chain, which is more than the whole lookup.
//
// SOUNDNESS. The entry CARRIES ITS OWN KEY, so a hit is a proof and a collision is a miss that
// falls through to exactly the code that ran before. Racing writers store IDENTICAL bytes (the
// answer is a function of the key alone), the word is written and read atomically so nothing can
// tear, and `Relaxed` suffices because the entry is self-contained — it points at nothing whose
// contents a reader would have to see. **Only POSITIVE answers are cached**, exactly as the
// `slot_memo` this fronts did: a class id that is not registered YET must keep re-asking, because
// its `None` is a "not yet", not a "never".
//
// SIZE, WHICH IS THE RISK AND IS STATED RATHER THAN GUESSED. `FXL1 = 1024` entries x 8 B =
// **8 KB of BSS, once, for the whole process** — half the bytes of the `slot_memo` this tree
// already ran (`[(u64, u32); 1024]` = 16 KB **per thread**) and shared instead of copied. A probe
// touches ONE 64 B line holding eight entries; the resident set is the hot (class, field) pairs,
// not the table. This tree has measured a **+6.8% regression** from a 14 KB per-thread front
// cache, which is why the entry is one word rather than a key/value pair and why the count is not
// larger. `an_fx_l1_entry_is_one_word` asserts the byte count so a later edit cannot double it.
const FXL1: usize = 1024;

/// `class id | field id | slot | kind` packed into one word. `0` = empty (see `fxl1_tag`).
static FX_L1: [AtomicU64; FXL1] = [const { AtomicU64::new(0) }; FXL1];

/// The KEY half of an entry: `field_id + 1` in bits 0..24, `class_id` in bits 24..40.
///
/// `+1` is what makes `0` an unambiguous empty slot: without it the perfectly legitimate answer
/// for `(class 0, field 0, slot 0, kind 0)` would be indistinguishable from a never-written word.
/// `None` = this pair cannot be represented, so it is never cached and never answered — the
/// caller keeps the mirror path, which is correct and merely the previous round's speed.
#[inline]
fn fxl1_tag(class_id: u32, field_id: u32) -> Option<u64> {
    if class_id >= 1 << 16 || field_id >= (1 << 24) - 1 {
        return None;
    }
    Some((field_id as u64 + 1) | ((class_id as u64) << 24))
}

/// Direct-mapped slot. Both halves are mixed in: a field id alone collides across every class
/// that interns it, and a class id alone collides across every field of one class. Same
/// multiply-shift the `slot_memo` this replaces used, on the same key.
#[inline]
fn fxl1_index(class_id: u32, field_id: u32) -> usize {
    let k = ((class_id as u64) << 32) | field_id as u64;
    (((k ^ (k >> 29)).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as usize) & (FXL1 - 1)
}

/// Read entry `idx`, answering only if its self-carried key equals `tag`.
///
/// Split from `fxl1_get` so the KEY and the SLOT INDEX — two pure functions of the same
/// `(class_id, field_id)` pair — are derived by the caller ONCE (see `find_slot_kind`).
#[inline]
fn fxl1_read(idx: usize, tag: u64) -> Option<(usize, u8)> {
    // SAFETY: the index is masked to `FXL1 - 1`, which is the array's exact length minus one.
    let e = unsafe { FX_L1.get_unchecked(idx) }.load(Ordering::Relaxed);
    // The low 40 bits are the key, compared IN FULL — a hit is a proof, never a hash guess.
    if e & 0xFF_FFFF_FFFF != tag {
        return None;
    }
    Some((((e >> 40) & 0xFFFF) as usize, (e >> 56) as u8))
}

/// Publish `(slot, kind)` under the already-derived `tag` at the already-derived `idx`.
#[inline]
fn fxl1_write(idx: usize, tag: u64, slot: usize, kind: u8) {
    // A slot past 16 bits is not representable here; refusing costs the mirror path, which is
    // what a class with 65,536 declared fields would have paid anyway.
    if slot >= 1 << 16 {
        return;
    }
    let e = tag | ((slot as u64) << 40) | ((kind as u64) << 56);
    // SAFETY: as `fxl1_read`.
    unsafe { FX_L1.get_unchecked(idx) }.store(e, Ordering::Relaxed);
}

/// ⭐**THE ANSWER BEFORE THE GATE** — the L1 probed WITHOUT reading the kill-switch word first.
/// (Still `RAVA_NO_FAST_RESOLVETL=1` / `emit_more_disabled()`; see the proof below for why those
/// need not be consulted to stay honest.)
///
/// `slot_of`, `kind_of` and `slot_kind_of` each opened with `gates_once()` — one
/// `OnceLock::get_or_init`, i.e. an **acquire load**, to learn a word that has not changed since
/// process start. On the HIT path that acquire is a third of the whole function and, worse, it is
/// the one instruction LLVM may not schedule around: EarlyCSE caches only `isUnordered()` loads,
/// so an ordered one bumps the pass's generation counter and DISCARDS the loads it had cached —
/// the caller's receiver word and layout base among them. That is the same fact the `GATE_NGTLS`
/// round measured one level up; `slot_of` is **1.21% of host callback samples** and this is what
/// stands in front of its one-load answer.
///
/// ⚠**WHY SKIPPING THE GATE IS SOUND, AND WHY IT MAY ONLY REJECT.** `FX_L1` is written in exactly
/// one place in a non-test build — `find_slot_kind`, under `g & GATE_FXL1 != 0`, and only reached
/// under `g & GATE_LAYOUT != 0`. The gate word is a process constant. So in a run with the block
/// disabled (either switch, or `emit_more_disabled()`) **the table is all zeros for the whole
/// process**, and a stored word can never be zero because `fxl1_tag` adds one to the field id ⇒
/// every probe MISSES and every caller falls through to the byte-for-byte code it ran before. A
/// disabled run therefore pays a bounded, named residue — two compares, the index mix, and one
/// load of a line that is Shared-clean in L1 — and takes the identical path.
/// In an ARMED run a hit is already a proof rather than a hash guess: the entry carries its own
/// 40-bit key and it is compared IN FULL, so the answer is exactly the one the gated path would
/// have read from the same entry. The `debug_assert` re-derives it from the class layout on every
/// hit in a debug build, which is how the differential probes check the claim rather than trust it.
#[inline]
fn fxl1_probe(class_id: u32, field_id: u32) -> Option<(usize, u8)> {
    let tag = fxl1_tag(class_id, field_id)?;
    let hit = fxl1_read(fxl1_index(class_id, field_id), tag)?;
    debug_assert_eq!(
        Some(hit),
        with_mirror_g(gates(), class_id, |m| m.find(field_id)).flatten(),
        "the (class, field) L1 answered ({class_id}, {field_id}) differently from the class layout"
    );
    Some(hit)
}

/// Key-and-index-deriving wrappers. `find_slot_kind` no longer uses them — it derives the pair
/// once and calls `fxl1_read`/`fxl1_write` — so these exist for the tests that pin the packing.
#[cfg(test)]
#[inline]
fn fxl1_get(class_id: u32, field_id: u32) -> Option<(usize, u8)> {
    fxl1_read(fxl1_index(class_id, field_id), fxl1_tag(class_id, field_id)?)
}

#[cfg(test)]
#[inline]
fn fxl1_put(class_id: u32, field_id: u32, slot: usize, kind: u8) {
    let Some(tag) = fxl1_tag(class_id, field_id) else { return };
    fxl1_write(fxl1_index(class_id, field_id), tag, slot, kind);
}

/// Slot AND kind of `(class_id, field_id)`, through the L1 when it is armed. ONE definition, so
/// `slot_of`, `kind_of` and `slot_kind_of` cannot drift apart about which tier answers them —
/// and so a `slot_of` that warms the L1 also serves the `kind_of` the same field access asks
/// immediately afterwards.
///
/// The cold arm is the caller's previous body verbatim: `with_mirror_g` -> `ClassMirror::find`.
#[inline]
fn find_slot_kind(g: u8, class_id: u32, field_id: u32) -> Option<(usize, u8)> {
    if g & GATE_FXL1 != 0 {
        // ⭐**ONE FACT, ONE DERIVATION.** The L1 TAG and the L1 SLOT INDEX are both pure functions
        // of the same `(class_id, field_id)` pair, and this path derived each of them TWICE on a
        // MISS: `fxl1_get` computed both, then `fxl1_put` computed both again — two 32-bit
        // multiplies, one 64-bit multiply, a shift/xor fold and two range tests, all to rebuild
        // words that were still live in registers. Population: every L1 miss on the arena
        // `getfield`/`putfield` and JIT field-callback paths, i.e. every first touch of a
        // `(class, field)` pair and every eviction of one — the compulsory half of a 1,024-entry
        // direct-mapped table `slot_of` (1.21% of host callback samples) sits behind.
        //
        // Derived once here and threaded into `fxl1_read`/`fxl1_write`. Bit-identical by
        // construction: both are deterministic functions of arguments nothing mutates in between.
        if let Some(tag) = fxl1_tag(class_id, field_id) {
            let idx = fxl1_index(class_id, field_id);
            if let Some(hit) = fxl1_read(idx, tag) {
                return Some(hit);
            }
            let ans = with_mirror_g(g, class_id, |m| m.find(field_id))?;
            if let Some((s, k)) = ans {
                fxl1_write(idx, tag, s, k);
            }
            return ans;
        }
        // Un-representable pair (class id ≥ 2^16 or field id at the 24-bit ceiling): never cached
        // and never answered from the table, so it takes the mirror path verbatim — which is what
        // `fxl1_tag`'s `None` already meant, spelled once instead of twice.
        return with_mirror_g(g, class_id, |m| m.find(field_id))?;
    }
    with_mirror_g(g, class_id, |m| m.find(field_id))?
}

/// Inline-cell slot of `field_id` in `class_id`'s layout, or None (field not in
/// the declared layout — a synthetic/append field, unsupported in Phase 2).
pub fn slot_of(class_id: u32, field_id: u32) -> Option<usize> {
    // ⭐**THE HIT PATH READS NO GATE AT ALL** — a hit in the L1 proves the gates that filled it
    // (see `fxl1_probe`), so the acquire load below is now paid only by a MISS. The key and index
    // are re-derived by `find_slot_kind` on that miss; six ALU ops in front of a three-dependent-
    // load mirror walk, deliberately accepted so the OFF arms stay byte-for-byte the previous code.
    if let Some((s, _)) = fxl1_probe(class_id, field_id) {
        return Some(s);
    }
    // Inlined rather than delegated to `slot_kind_of` so the gate is read ONCE per access — and
    // now that "once" is literal: `gates_once` reads the word here and `with_mirror_g` tests the
    // other two bits against the register instead of taking two more acquire loads.
    if let Some(g) = gates_once() {
        if g & GATE_LAYOUT != 0 {
            return find_slot_kind(g, class_id, field_id).map(|(s, _)| s);
        }
    } else if fast_layout() {
        return with_mirror(class_id, |m| m.find(field_id))?.map(|(s, _)| s);
    }
    // ── kill-switch arm: the pre-round code, memo and all ──────────────────────────────────
    if let Some(s) = slot_memo::get(class_id, field_id) {
        return Some(s);
    }
    let hit = with_class(class_id, |_, l| l.iter().position(|&(id, _)| id == field_id))?;
    if let Some(s) = hit {
        slot_memo::put(class_id, field_id, s);
    }
    hit
}

/// SLOT AND KIND IN ONE LOOKUP — the arena get/putfield paths ask both questions about the same
/// `(class, field)` and, before this, paid two independent derivations for them (`slot_of` then
/// `kind_of`, the second of which was not even memoised). Both are constants of the class, and
/// the index stores them in the SAME 8-byte entry, so the pair costs exactly one probe.
///
/// Prefer this over calling `slot_of` and `kind_of` back to back. HAND-OFF: `opcodes::arena_cells`
/// (not this agent's file) does exactly that, twice — once in the get path and once in the put.
#[allow(dead_code)] // the second consumer lives in a file this round did not own
pub fn slot_kind_of(class_id: u32, field_id: u32) -> Option<(usize, u8)> {
    // As `slot_of`: a hit answers both halves without reading the gate word (see `fxl1_probe`).
    if let Some(hit) = fxl1_probe(class_id, field_id) {
        return Some(hit);
    }
    if let Some(g) = gates_once() {
        if g & GATE_LAYOUT != 0 {
            return find_slot_kind(g, class_id, field_id);
        }
    } else if fast_layout() {
        return with_mirror(class_id, |m| m.find(field_id))?;
    }
    let s = slot_of(class_id, field_id)?;
    let k = kind_of(class_id, field_id)?;
    Some((s, k))
}

/// SLOT MEMO — `slot_of` is a LINEAR SCAN of the class layout inside a thread-local mirror
/// lookup, and it runs on EVERY arena field read. The handle side has had a slot hint for as
/// long as `get_field_hint` has existed; the arena side re-derived the answer every time.
///
/// ⭐**Why it matters beyond this function**: ledger row 325 leaves ~25 s of the arena migration's
/// +29.7 s unattributed after host-transition counts (~290 ns each) and GC (measured: `gc_time`
/// 185.5 → 245.7 ms, i.e. +60 ms — refuted) both came up short. Host `arena_getfield` rises
/// **20.3 M → 36.8 M** across that flag, and this scan is what each one pays.
///
/// ⚠**ONLY POSITIVE ANSWERS ARE CACHED.** A layout can gain fields as classes register, so a
/// `None` may become a `Some` later and must be re-asked; a `Some` cannot move, because a layout
/// is only ever APPENDED to and a slot index is a position in it. Direct-mapped, thread-local,
/// no lock — the same shape as `ARM_CACHE`, for the same reason (row 178: the cost in this layer
/// is locks and hashes).
mod slot_memo {
    const SLOTS: usize = 1024;
    thread_local! {
        static T: std::cell::RefCell<[(u64, u32); SLOTS]> =
            const { std::cell::RefCell::new([(u64::MAX, 0); SLOTS]) };
    }

    #[inline]
    fn key(class_id: u32, field_id: u32) -> u64 {
        ((class_id as u64) << 32) | field_id as u64
    }

    #[inline]
    fn index(k: u64) -> usize {
        // Mix both halves in: a field id alone collides across every class that shares it, and
        // a class id alone collides across every field of one class.
        ((k ^ (k >> 29)).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as usize & (SLOTS - 1)
    }

    #[inline]
    pub(super) fn get(class_id: u32, field_id: u32) -> Option<usize> {
        let k = key(class_id, field_id);
        T.with(|c| {
            let t = c.borrow();
            let e = t[index(k)];
            (e.0 == k).then_some(e.1 as usize)
        })
    }

    #[inline]
    pub(super) fn put(class_id: u32, field_id: u32, slot: usize) {
        let k = key(class_id, field_id);
        T.with(|c| c.borrow_mut()[index(k)] = (k, slot as u32));
    }
}

/// Diagnostic: the full layout (field_id, kind) list for a class — to inspect why a
/// putfield/getfield id misses the layout (RAVA_AOT diagnostics).
pub fn layout_dump(class_id: u32) -> Vec<(u32, u8)> {
    let r = registry().lock().unwrap();
    r.get(class_id as usize).map(|c| c.layout.to_vec()).unwrap_or_default()
}

/// Kind byte of `field_id` in `class_id` (FK_*), for decode.
///
/// WAS an UNMEMOISED linear scan of the layout on every arena getfield — the one lookup in this
/// family that had no memo at all, so it re-derived a per-class constant per ACCESS, growing with
/// fields per object. IS one probe of the per-class index.
pub fn kind_of(class_id: u32, field_id: u32) -> Option<u8> {
    // As `slot_of`: a hit answers without reading the gate word (see `fxl1_probe`). The same entry
    // a `slot_of` on this pair seeded serves the `kind_of` the field access asks right after.
    if let Some((_, k)) = fxl1_probe(class_id, field_id) {
        return Some(k);
    }
    if let Some(g) = gates_once() {
        if g & GATE_LAYOUT != 0 {
            return find_slot_kind(g, class_id, field_id).map(|(_, k)| k);
        }
    } else if fast_layout() {
        return with_mirror(class_id, |m| m.find(field_id))?.map(|(_, k)| k);
    }
    with_class(class_id, |_, l| l.iter().find(|&&(id, _)| id == field_id).map(|&(_, k)| k))?
}

/// `kind_of` FOR A CALLER THAT ALREADY HOLDS THE GATE WORD.
///
/// Exactly `kind_of`, with the `gates_once()` acquire load removed because the caller made it.
/// `kind_of` is `gates_once()` (⇒ `Some(g)` iff `GATE_NGTLS`) then `g & GATE_LAYOUT`, so
/// `kind_of_g(gates_word(), c, f) == kind_of(c, f)` for every input by construction — the word is
/// a process constant written once, so reading it once and reading it twice cannot disagree.
///
/// Used by `nextgen::lambdas::arena_lambda_cell`, which needs the arena-derive bit and a field
/// kind on the SAME call and must not pay two acquire loads for one process constant.
#[inline]
pub(crate) fn kind_of_g(g: u8, class_id: u32, field_id: u32) -> Option<u8> {
    if g & GATE_NGTLS != 0 {
        if g & GATE_LAYOUT != 0 {
            return find_slot_kind(g, class_id, field_id).map(|(_, k)| k);
        }
    } else if g & GATE_LAYOUT != 0 {
        return with_mirror(class_id, |m| m.find(field_id))?.map(|(_, k)| k);
    }
    with_class(class_id, |_, l| l.iter().find(|&&(id, _)| id == field_id).map(|&(_, k)| k))?
}

/// `(field_id, kind)` at inline slot `i` of `class_id`'s layout — the inverse of
/// `slot_of`, for diagnostics that have a slot index and want the declared field
/// id (correlates a raw cell scan back to a `getfield id=N` panic).
pub fn field_id_at(class_id: u32, i: usize) -> Option<(u32, u8)> {
    with_class(class_id, |_, l| l.get(i).copied())?
}

// ═══════════════ ArenaString — an inline-byte string on the arena ═══════════════
// A Java String stored as a REGULAR arena object of a dedicated, ref-mask-EMPTY
// class, so the Cheney collector copies it (`obj_size(nf)` bytes) and SKIPS its
// cells (ref_mask 0 → the byte data is never mis-traced as pointers). NO GC-core
// change. It is a `Value::Obj` whose `obj_class == string_class_id()`. Layout:
//   [0]=class  [4]=nfields  [8]=fwd  [16]=identity-hash  (standard 24B header)
//   [24] = byte length (i32)   [28] = CHAR length (i32, == Java length())
//   [32] = cached Java hashCode (i32)
//   [40..] = the UTF-8 bytes, inline, zero-padded to an 8-byte cell boundary
// `nfields = 2 (the len + hash cells) + ceil(byte_len / 8)` so `obj_size` covers
// the inline bytes. `raw_u8_ptr` for native JIT access is `obj + 40` (computed,
// never cached across a GC since the object moves with its bytes).

/// The `(class, field) -> (slot, kind)` L1 answers from a word that carries its own key, so the
/// two things that can go wrong are an entry that is confused with an EMPTY slot and a colliding
/// pair that is answered instead of missing. Both are pinned here; the size claim is too, because
/// a table that stops fitting in L1 is the +6.8% regression this tree has already measured once.
#[cfg(test)]
mod fxl1_tests {
    use super::*;

    #[test]
    fn an_fx_l1_entry_is_one_word() {
        assert_eq!(std::mem::size_of::<AtomicU64>(), 8);
        assert_eq!(FXL1, 1024);
        assert_eq!(std::mem::size_of_val(&FX_L1), 8192, "the L1 must stay 8 KB, process-wide");
    }

    /// `(class 0, field 0, slot 0, kind 0)` is a legitimate answer whose naive packing is the
    /// all-zero word — i.e. exactly the never-written slot. The `+1` on the field id is what
    /// keeps them apart, and nothing else in the suite would notice if it were dropped.
    #[test]
    fn the_zero_answer_is_not_an_empty_slot() {
        let tag = fxl1_tag(0, 0).expect("representable");
        assert_ne!(tag, 0, "an occupied slot must never encode as the empty word");
        // Un-representable keys are refused rather than truncated into someone else's entry.
        assert!(fxl1_tag(1 << 16, 0).is_none());
        assert!(fxl1_tag(0, (1 << 24) - 1).is_none());
    }

    /// A pair that lands in an occupied slot must MISS and fall through, never read the
    /// occupant's answer — the key is compared in full for exactly this reason.
    #[test]
    fn a_colliding_pair_misses_rather_than_answering() {
        // Two distinct pairs in the same slot, found by construction rather than assumed. The
        // class ids are deliberately near the 16-bit ceiling: `FX_L1` is process-global, so a
        // test that wrote a REGISTERED class's id could answer another test's `slot_of`.
        let a = (0xFFF0u32, 11u32);
        let b = (1u32..200_000)
            .map(|f| (a.0, f))
            .find(|&p| p.1 != a.1 && fxl1_index(p.0, p.1) == fxl1_index(a.0, a.1))
            .expect("1024 slots over 200k field ids must collide");
        fxl1_put(a.0, a.1, 3, 6);
        assert_eq!(fxl1_get(a.0, a.1), Some((3, 6)));
        // B evicts A: B answers, A misses.
        fxl1_put(b.0, b.1, 9, 1);
        assert_eq!(fxl1_get(b.0, b.1), Some((9, 1)));
        assert_eq!(fxl1_get(a.0, a.1), None, "an evicted pair must miss, not read B's answer");
        // A different CLASS at the same field must miss too (the other half of the key).
        assert_eq!(fxl1_get(a.0 + 1, b.1), None);
        // And the packing round-trips the extremes it claims to represent.
        fxl1_put(0xFFFF, 0xFF_FFFD, 0xFFFF, 0xFF);
        assert_eq!(fxl1_get(0xFFFF, 0xFF_FFFD), Some((0xFFFF, 0xFF)));
    }
}
