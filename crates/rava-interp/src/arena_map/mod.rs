//! Flat-array open-addressing HashMap on the arena (collections migration, Step 1).
//!
//! Flat-array open-addressing `HashMap` on the GC arena (next-gen-gc, Step 1).
//!
//! This is the storage backend the runtime is migrating `java/util/HashMap` onto,
//! replacing the Rust-side `std::HashMap` hidden behind the `$map` native field.
//! Goal: erase the FFI/callback boundary so a later step can emit the probe loop
//! straight into Cranelift IR against arena memory.
//!
//! ## Guest structure (all on the arena)
//! The map is a `Value::Obj` of class `rava/ArenaMap` with four inline cells:
//! ```text
//!   [0] capacity  (i32, power of two)   [1] size  (i32, live entries)
//!   [2] table     (ref → table object)  [3] used  (i32, occupied + tombstone)
//! ```
//! `table` is itself an arena object (class `rava/ArenaMapTable$<cap>`) holding
//! `3 * capacity` inline cells — one open-addressing bucket per `3` cells:
//! ```text
//!   [3i+0] head   = (hash:i32) << 2 | state   state: 0 EMPTY · 1 OCCUPIED · 2 TOMBSTONE
//!   [3i+1] key    (GC-safe map cell)
//!   [3i+2] value  (GC-safe map cell)
//! ```
//!
//! ## GC correctness
//! The collector follows any ref-masked cell whose low 2 bits are `0b00` as a young
//! pointer (see `nextgen` Cheney scan). A raw `Value::Int` would therefore be
//! mis-traced. So the table class masks ONLY the key/value cells (the head cell is
//! a primitive `i32`, never traced), and those cells use `encode_map_cell`, which —
//! unlike `encode_arena_cell` — boxes primitives/strings through the arena box
//! table (tag `0b11`). Every non-`Obj` value is therefore tagged (`0b01`/`0b10`/
//! `0b11`) or null and skipped by the collector; only a real arena object pointer
//! (`0b00`, non-null) is forwarded. The table is reachable through the map's
//! ref-masked `table` cell, so a live map keeps its whole table + entries alive.




#![cfg(feature = "next-gen-gc")]

use crate::arena::ArenaPtr;
use crate::heap::MapKey;
use crate::nextgen;
use crate::value::Value;
use crate::VmError;
use std::sync::{Arc, OnceLock};

/// Native-dispatch result: `Ok(None)` not handled · `Ok(Some(None))` void ·
/// `Ok(Some(Some(v)))` a returned value (same convention as the other backends).
type Handled = Result<Option<Option<Value>>, VmError>;

// ─────────────────────── GC-protected temporaries (handle scope) ───────────────────────
// In-flight values during a map op — a computed callback result, the previous
// value being returned, the key/map themselves — live only in Rust locals, so a
// collection triggered by a resize OR inside a guest lambda would reclaim them
// (they are not Frame/static roots). They are pushed here for the duration: the
// collector forwards every protected slot in place (see `forward_temps`, called
// from `Vm::arena_collect_jit`), so a map op reads the FORWARDED value back after
// any collect. Strict stack discipline — push, (maybe collect), read back, pop.

thread_local! {
    pub(crate) static GC_TEMPS: std::cell::RefCell<Vec<Value>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The collector forwards each protected temporary in place.
pub(crate) fn forward_temps(f: &mut dyn FnMut(&mut Value)) {
    GC_TEMPS.with(|t| {
        for v in t.borrow_mut().iter_mut() {
            f(v);
        }
    });
}

/// Protect `v` across a potential collection; returns its handle index.
fn protect(v: Value) -> usize {
    GC_TEMPS.with(|t| {
        let mut b = t.borrow_mut();
        b.push(v);
        b.len() - 1
    })
}
/// Read a protected value (forwarded if a collection moved it).
fn protected(i: usize) -> Value {
    GC_TEMPS.with(|t| t.borrow()[i].clone())
}
/// Drop every protection at/after `base` (the scope's high-water mark).
fn unprotect_to(base: usize) {
    GC_TEMPS.with(|t| t.borrow_mut().truncate(base));
}
fn temps_mark() -> usize {
    GC_TEMPS.with(|t| t.borrow().len())
}

/// Restore the arena-map temporary-root stack on every exit path, including `?`.
pub(crate) struct TempScope(pub(crate) usize);

impl Drop for TempScope {
    fn drop(&mut self) {
        unprotect_to(self.0);
    }
}
/// A protected arena map read back as its (forwarded) pointer.
fn protected_map(i: usize) -> ArenaPtr {
    match protected(i) {
        Value::Obj(p) => p,
        _ => unreachable!("a protected map slot always holds an arena Obj"),
    }
}

// Map object cell indices.
const CAP: usize = 0;
const SIZE: usize = 1;
const TABLE: usize = 2;
const USED: usize = 3;
// fastutil maps carry a configurable "default return value" returned by get() for an
// absent key (java.util maps leave it null). Stored as a GC-traced map cell; the slot
// is zero-initialised → Value::Null, the correct default for both.
const DEFAULT: usize = 4;

// ───────────────────────────── Split-cell bucket layout ─────────────────────────────
// Each bucket is FOUR consecutive arena cells:
//   [4b+0] head    = (hash:i32) << 3 | INT_KEY << 2 | state   state: 0 EMPTY 1 OCC 2 TOMB
//   [4b+1] key_int (raw i64 — a primitive, NEVER traced; valid iff INT_KEY)
//   [4b+2] key_obj (GC-traced map cell — the object key, else null/0)
//   [4b+3] value   (GC-traced map cell)
// The INT_KEY bit lets the JIT compare a primitive key with a single register
// `icmp` against key_int, with zero FFI — no box-table unboxing. The collector
// traces only key_obj + value (their cells are ref-masked); head + key_int are
// primitive cells (ref-mask 0), so the raw int is never mis-read as a pointer.
const BW: usize = 4; // bucket width, in cells

// Bucket state, stored in the low 2 bits of the head cell.
const EMPTY: i64 = 0;
const OCC: i64 = 1;
const TOMB: i64 = 2;

#[inline]
fn head_state(h: i64) -> i64 {
    h & 3
}
#[inline]
fn head_is_int(h: i64) -> bool {
    (h >> 2) & 1 != 0
}
#[inline]
fn head_hash(h: i64) -> i32 {
    (h >> 3) as i32
}
#[inline]
fn make_head(hash: i32, is_int: bool, state: i64) -> i64 {
    ((hash as i64) << 3) | ((is_int as i64) << 2) | state
}

/// Smallest table capacity (a power of two); also the default for a fresh map.
const MIN_CAP: usize = 16;

/// The arena `HashMap` class id (4 cells: cap, size, table-ref, used). Registered
/// under the real guest name `java/util/HashMap` so a virtual call on the receiver
/// resolves to no bytecode → the native layer → `map::dispatch` → the arena route,
/// and `instanceof java/util/Map` etc. answer correctly. The class is only ever
/// instantiated by `new_map` (the flagged allocation switch), so `is_arena_map`'s
/// class-id check uniquely identifies our flat maps.
fn map_class() -> u32 {
    pub(crate) static C: OnceLock<u32> = OnceLock::new();
    *C.get_or_init(|| {
        let layout: Arc<[(u32, u8)]> =
            vec![(0u32, 0u8), (1, 0), (2, 6 /* ref */), (3, 0), (4, 6 /* default: ref */)].into();
        nextgen::register_class(&(Arc::from("java/util/HashMap") as Arc<str>), &layout)
    })
}

/// The table class for a given (power-of-two) capacity: `4*cap` cells. The Cheney
/// scanner traces exactly the ref-masked cells, so head (primitive) and key_int
/// (raw `long` primitive) are skipped, while key_obj + value (kind 6) are traced
/// and forwarded. `register_class` interns by name → registered once per capacity.
fn table_class(cap: usize) -> u32 {
    let name: Arc<str> = Arc::from(format!("rava/ArenaMapTable4${cap}"));
    let mut layout: Vec<(u32, u8)> = Vec::with_capacity(cap * BW);
    for i in 0..cap {
        let b = (i * BW) as u32;
        layout.push((b, 0)); // head: primitive
        layout.push((b + 1, 1 /* long */)); // key_int: primitive, NOT traced
        layout.push((b + 2, 6 /* ref */)); // key_obj: traced
        layout.push((b + 3, 6 /* ref */)); // value: traced
    }
    nextgen::register_class(&name, &(layout.into()))
}

/// Hash a resolved map key, spreading high bits down (the table masks by
/// `capacity-1`, so unspread hashes would cluster). Matches `HashMap`'s mixing.
fn hash_of(mk: &MapKey) -> i32 {
    let h = match mk {
        MapKey::Int(n) => (*n ^ (*n >> 32)) as i32,
        MapKey::Str(s) => s
            .bytes()
            .fold(0i32, |a, b| a.wrapping_mul(31).wrapping_add(b as i32)),
        // Combo-map fold: a non-int, non-string key. bucket_matches re-derives the
        // stored key Value's MapKey (value_map_key → Hash again) and compares, so
        // storage/compare need nothing special beyond this hash spread.
        MapKey::Hash(u) => (*u ^ (*u >> 32)) as i32,
        // Alloc-free identity key — same treatment as the combo fold: spread and compare.
        MapKey::Id(u) => (*u ^ (*u >> 32)) as i32,
    };
    h ^ (((h as u32) >> 16) as i32)
}

#[inline]
fn read(p: ArenaPtr, slot: usize) -> i64 {
    nextgen::arena_cell(p, slot)
}
#[inline]
fn write(p: ArenaPtr, slot: usize, v: i64) {
    nextgen::arena_set_cell(p, slot, v);
}
#[inline]
fn table_of(map: ArenaPtr) -> ArenaPtr {
    unsafe { ArenaPtr::from_addr(read(map, TABLE) as u64) }
}

/// Allocate a fresh, zeroed table object of `cap` buckets. `None` on nursery
/// overflow (the caller must collect + retry — handled at the live seam in Step 2).
fn alloc_table(cap: usize) -> Option<ArenaPtr> {
    nextgen::tlab_try_alloc(table_class(cap), (cap * BW) as u32)
}

/// Allocate a fresh empty `ArenaMap` with the minimum capacity. `None` on overflow.
pub fn new_map() -> Option<ArenaPtr> {
    new_with_cap(MIN_CAP)
}

fn new_with_cap(cap: usize) -> Option<ArenaPtr> {
    let table = alloc_table(cap)?;
    let map = nextgen::tlab_try_alloc(map_class(), 5)?;
    write(map, CAP, cap as i64);
    write(map, SIZE, 0);
    write(map, TABLE, table.addr() as i64);
    write(map, USED, 0);
    write(map, DEFAULT, 0); // default return = Value::Null
    Some(map)
}

/// Does bucket `slot` (already known OCCUPIED with a matching hash) hold a key
/// equal to the query `mk`? An int bucket compares its raw `key_int` (no decode,
/// no allocation); an object bucket compares the stored object's resolved key. A
/// MapKey::Int query never equals an object key and vice-versa (distinct variants),
/// so the kinds short-circuit before any object work.
fn bucket_matches(vm: &mut crate::Vm, table: ArenaPtr, slot: usize, head: i64, mk: &MapKey) -> bool {
    if head_is_int(head) {
        matches!(mk, MapKey::Int(qn) if read(table, slot * BW + 1) == *qn)
    } else if matches!(mk, MapKey::Int(_)) {
        false
    } else {
        let sk = decode_map_cell(read(table, slot * BW + 2));
        crate::map::value_map_key(vm, &sk) == *mk
    }
}

/// Write a key/value into bucket `slot` as OCCUPIED. An `Int` key goes raw into
/// `key_int` with the head INT_KEY bit set (and `key_obj` cleared); any other key
/// is GC-encoded into `key_obj`.
fn write_bucket(table: ArenaPtr, slot: usize, h: i32, key: &Value, mk: &MapKey, val: &Value) {
    let is_int = matches!(mk, MapKey::Int(_));
    write(table, slot * BW, make_head(h, is_int, OCC));
    write(table, slot * BW + 1, if let MapKey::Int(n) = mk { *n } else { 0 });
    write(table, slot * BW + 2, if is_int { 0 } else { encode_map_cell(key) });
    write(table, slot * BW + 3, encode_map_cell(val));
}

/// Reconstruct an OCCUPIED bucket's `(key, value)` — `None` for EMPTY/TOMBSTONE.
/// An int bucket rebuilds `Value::Int(key_int)` (a `MapKey::Int` only ever came
/// from a `Value::Int`, so the i32 round-trips); an object bucket decodes key_obj.
fn bucket_entry(table: ArenaPtr, b: usize) -> Option<(Value, Value)> {
    let head = read(table, b * BW);
    if head_state(head) != OCC {
        return None;
    }
    let key = if head_is_int(head) {
        Value::Int(read(table, b * BW + 1) as i32)
    } else {
        decode_map_cell(read(table, b * BW + 2))
    };
    Some((key, decode_map_cell(read(table, b * BW + 3))))
}

/// The configured default-return value (fastutil) — `Null` unless `defaultReturnValue`
/// set it. Returned by `get` for an absent key.
pub fn default_return(map: ArenaPtr) -> Value {
    decode_map_cell(read(map, DEFAULT))
}

/// `map.put(key, value)` — returns the previous value (or `Null`). Grows + rehashes
/// when the table gets dense (`used*4 >= cap*3`), so an EMPTY slot always exists and
/// the probe terminates. `None` only on nursery overflow during a grow.
pub fn put(vm: &mut crate::Vm, map: &mut ArenaPtr, key: Value, val: Value) -> Option<Value> {
    // This legacy entry grows before inserting. Rehash key derivation can collect, so the
    // not-yet-stored key/value need their own roots until `insert` publishes them.
    let base = temps_mark();
    let mi = protect(Value::Obj(*map));
    let ki = protect(key);
    let vi = protect(val);
    let mut current = protected_map(mi);
    let used = read(current, USED) as usize;
    let cap = read(current, CAP) as usize;
    if (used + 1) * 4 >= cap * 3 {
        if grow(vm, &mut current, (cap * 2).max(MIN_CAP)).is_none() {
            *map = protected_map(mi);
            unprotect_to(base);
            return None;
        }
    }
    let out = insert(vm, &mut current, protected(ki), protected(vi));
    *map = protected_map(mi);
    unprotect_to(base);
    Some(out)
}

pub fn size(map: ArenaPtr) -> i32 {
    read(map, SIZE) as i32
}

/// Is `p` one of our `rava/ArenaMap` objects?
///
/// Validates `p` is a real arena object pointer BEFORE reading its header: a
/// `Value::Obj` reaching the interpreter map dispatch can be a stray small value
/// (e.g. `4`) that the `>>40` discriminator wrongly accepts as arena — dereferencing
/// it faults. A non-arena address is definitionally not one of our maps → `false`.
pub fn is_arena_map(p: ArenaPtr) -> bool {
    nextgen::is_arena_obj_addr(p.addr()) && nextgen::arena_class_id(p) == map_class()
}

/// The ArenaMap class id — baked as a constant into the JIT probe loop's guard so
/// it can confirm an arbitrary receiver is one of our maps before dereferencing.
pub fn class_id() -> u32 {
    map_class()
}

/// Decode a table VALUE cell (map encoding) into a JIT operand cell. Used by the
/// inline IR probe ONLY for an ABOX-tagged value (a boxed primitive/String) on a
/// hit — handle/obj/null values are converted inline in IR. `box_ref`-style boxing
/// uses the JIT box table (no arena allocation → GC-free, no spill needed).
#[cfg(feature = "next-gen-gc")]
pub fn jit_box_value_cell(vm: &mut crate::Vm, cell: i64) -> i64 {
    let v = decode_map_cell(cell);
    crate::jit::box_ref(vm, &v)
}

/// Current location of `map` after a possible collection: a moved object carries a
/// forwarding pointer (`copy_obj` sets it); an untouched one reads null → unchanged.
fn relocate(map: ArenaPtr) -> ArenaPtr {
    let f = unsafe { nextgen::obj_fwd(map.as_ptr()) };
    if f.is_null() {
        map
    } else {
        unsafe { ArenaPtr::new(f).expect("forwarded pointer is non-null") }
    }
}

/// Snapshot every live entry by walking the table, skipping EMPTY/TOMBSTONE — the
/// bridge that lets `keySet`/`values`/`entrySet`/`forEach`/… reuse the existing
/// heap iterator machinery unchanged.
pub fn entries(map: ArenaPtr) -> Vec<(Value, Value)> {
    let cap = read(map, CAP) as usize;
    let table = table_of(map);
    let mut out = Vec::with_capacity(read(map, SIZE).max(0) as usize);
    for b in 0..cap {
        if let Some(e) = bucket_entry(table, b) {
            out.push(e);
        }
    }
    out
}

/// Empty the map without reallocating: re-EMPTY every bucket, reset counters.
pub(crate) fn clear(map: ArenaPtr) {
    let cap = read(map, CAP) as usize;
    let table = table_of(map);
    for s in 0..cap * BW {
        write(table, s, 0);
    }
    write(map, SIZE, 0);
    write(map, USED, 0);
}

/// Store-or-remove tail shared by compute*/merge. `ki`/`vi` are protected handles
/// for the key and the computed new value (each forwarded if a collection moved
/// it). A null new value removes the key and returns null; otherwise it stores the
/// value and returns it (re-read forwarded, since `live_put`'s resize may collect).
/// `*map` is updated to its post-resize location.
fn finish_compute(vm: &mut crate::Vm, map: &mut ArenaPtr, ki: usize, vi: usize) -> Value {
    if matches!(protected(vi), Value::Null) {
        let key = protected(ki);
        remove(vm, map, &key);
        Value::Null
    } else {
        live_put(vm, map, protected(ki), protected(vi));
        protected(vi)
    }
}

/// Entries of a source map argument (for copy-constructor / `putAll`), whether it
/// is an arena map or a legacy `$map` handle.
fn source_entries(vm: &mut crate::Vm, src: &Value) -> Vec<(Value, Value)> {
    match src {
        Value::Obj(p) if is_arena_map(*p) => entries(*p),
        Value::Ref(h) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Map) {
            Value::Int(id) => vm.heap.map_entries(id as u32),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}
mod dispatch;
pub(crate) use dispatch::*;
pub(crate) mod ops;
pub(crate) use ops::*;
mod tests;
pub(crate) use tests::*;
mod table;
pub(crate) use table::*;
