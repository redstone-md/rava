//! The open-addressed table: probing, growth and the cell encoding.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::arena_map::*;

/// GC-safe encoding of a key/value into a ref-masked table cell. Primitives and
/// strings are boxed (tag `0b11`) so the collector never reads them as pointers;
/// handles are tagged (`0b01`/`0b10`); a raw arena object passes through (`0b00`).
pub(crate) fn encode_map_cell(v: &Value) -> i64 {
    match v {
        Value::Null => 0,
        Value::Obj(p) => p.addr() as i64,
        Value::Ref(h) => ((*h as i64) << 2) | 1,
        Value::Array(a) => ((*a as i64) << 2) | 2,
        // Small int INLINE in the `0b11` (box) tag space — bit 2 set marks an inline int
        // (sign-extended payload in bits 3+), avoiding the `arena_box_push` allocation a
        // boxed primitive would cost. The collector skips ALL `0b11` cells (they are
        // never pointers), so this is GC-safe by construction. Other primitives/strings
        // still box (bit 2 clear → side-table index in bits 3+).
        Value::Int(i) => ((*i as i64) << 3) | 0b111,
        _ => ((nextgen::arena_box_push(v.clone()) as i64) << 3) | 0b011,
    }
}

/// Inverse of `encode_map_cell`.
pub(crate) fn decode_map_cell(cell: i64) -> Value {
    if cell == 0 {
        return Value::Null;
    }
    match cell & 3 {
        1 => Value::Ref((cell >> 2) as u32),
        2 => Value::Array((cell >> 2) as u32),
        3 => {
            if cell & 0b100 != 0 {
                Value::Int((cell >> 3) as i32) // inline small int
            } else {
                nextgen::arena_box_get((cell >> 3) as usize)
            }
        }
        _ => Value::Obj(unsafe { ArenaPtr::from_addr(cell as u64) }),
    }
}

/// `map.get(key)` — linear probe; returns the stored value or the default-return
/// (Null for a java.util map, the fastutil `defaultReturnValue` otherwise) if absent.
pub fn get(vm: &mut crate::Vm, map: &mut ArenaPtr, key: &Value) -> Value {
    let base = temps_mark();
    let mi = protect(Value::Obj(*map));
    let ki = protect(key.clone());
    let key = protected(ki);
    let mk = crate::map::value_map_key(vm, &key);
    let current = protected_map(mi);
    let cap = read(current, CAP) as usize;
    if cap == 0 {
        let out = default_return(current);
        *map = current;
        unprotect_to(base);
        return out;
    }
    let h = hash_of(&mk);
    let mask = cap - 1;
    let mut idx = (h as u32 as usize) & mask;
    let out = (|| {
      for _ in 0..cap {
        let current = protected_map(mi);
        let table = table_of(current);
        let head = read(table, idx * BW);
        match head_state(head) {
            EMPTY => return default_return(current),
            OCC if head_hash(head) == h && bucket_matches(vm, table, idx, head, &mk) => {
                // The stored-key derivation can collect. Read the value from the
                // forwarded table, never from the pointer passed to `bucket_matches`.
                let table = table_of(protected_map(mi));
                return decode_map_cell(read(table, idx * BW + 3));
            }
            _ => {} // empty-not-reached, tombstone, hash/key mismatch → keep probing
        }
        idx = (idx + 1) & mask;
      }
      default_return(protected_map(mi))
    })();
    *map = protected_map(mi);
    unprotect_to(base);
    out
}

/// `map.containsKey(key)` via the presence check (distinct from `get != null`).
pub fn contains(vm: &mut crate::Vm, map: &mut ArenaPtr, key: &Value) -> bool {
    let base = temps_mark();
    let mi = protect(Value::Obj(*map));
    let ki = protect(key.clone());
    let key = protected(ki);
    let mk = crate::map::value_map_key(vm, &key);
    let current = protected_map(mi);
    let cap = read(current, CAP) as usize;
    if cap == 0 {
        *map = current;
        unprotect_to(base);
        return false;
    }
    let h = hash_of(&mk);
    let mask = cap - 1;
    let mut idx = (h as u32 as usize) & mask;
    let out = (|| {
      for _ in 0..cap {
        let table = table_of(protected_map(mi));
        let head = read(table, idx * BW);
        match head_state(head) {
            EMPTY => return false,
            OCC if head_hash(head) == h && bucket_matches(vm, table, idx, head, &mk) => {
                return true;
            }
            _ => {}
        }
        idx = (idx + 1) & mask;
      }
      false
    })();
    *map = protected_map(mi);
    unprotect_to(base);
    out
}

/// Grow to `new_cap` and rehash every live entry into a fresh table (which drops
/// all tombstones). `None` on nursery overflow allocating the new table.
pub(crate) fn grow(vm: &mut crate::Vm, map: &mut ArenaPtr, new_cap: usize) -> Option<()> {
    let base = temps_mark();
    let mi = protect(Value::Obj(*map));
    let current = protected_map(mi);
    let old_cap = read(current, CAP) as usize;
    let old_table = table_of(current);
    let new_table = match alloc_table(new_cap) {
        Some(table) => table,
        None => {
            unprotect_to(base);
            return None;
        }
    };
    // Detaching the old table would otherwise leave it reachable only through a raw
    // Rust pointer while rehash key derivation is free to collect.
    let oi = protect(Value::Obj(old_table));
    let mut current = protected_map(mi);
    write(current, TABLE, new_table.addr() as i64);
    write(current, CAP, new_cap as i64);
    write(current, SIZE, 0);
    write(current, USED, 0);
    for b in 0..old_cap {
        let old_table = protected_map(oi);
        if let Some((k, v)) = bucket_entry(old_table, b) {
            insert(vm, &mut current, k, v);
        }
    }
    *map = protected_map(mi);
    unprotect_to(base);
    Some(())
}

// ══════════════════════ Live runtime surface (interpreter) ══════════════════════
// The cut-over wires `java/util/HashMap` onto the structure above. ✅**DEFAULT ON since
// 2026-09-04** (`RAVA_NO_ARENA_MAPS=1` restores the byte-identical `$map` path).
//
// ⭐**THE +48% IN THE LEDGER WAS STALE, AND IT HAD BEEN TREATED AS A LOCKED DOOR.** That number is
// the reason the "remove the handle heap" line was recorded as not-startable: arena collections
// had to come first and they cost 48% of a boot. Re-measured on today's engine, interleaved, 4
// rounds: wall ON 32.0/32.2/29.6/30.2 against OFF 32.5/36.2/36.8/35.2 and CPU ON
// 35.1/35.3/32.4/33.6 against OFF 35.5/39.4/40.1/38.1 — **every ON below every OFF on BOTH,
// DISJOINT, CPU median −11.35%**. Counted alongside it: arena-lambda births per warm reload
// 1,387,143 → 302,879 (**−78%**) and GC time 370.8 → 312.3 ms, with `ReloadBench` returning
// `HASH=c956377e19071d7a` in both arms.
//
// The live
// path runs through the INTERPRETER dispatch, where the map is a tracked GC root
// (an operand on a real `Frame`), so a resize collection forwards it and we
// re-locate via its forwarding pointer. The JIT map intrinsics are disabled under
// the flag (Step 3 emits the register probe loop); until then maps run interpreted.

/// True when the arena-map backend is live (env `RAVA_ARENA_MAPS`). Cached once.
pub fn enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    pub(crate) static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            // Default ON; the kill switch is the NEGATIVE variable. `RAVA_ARENA_MAPS=1` stays
            // accepted so every script and note that sets it keeps meaning what it meant.
            let on = std::env::var_os("RAVA_NO_ARENA_MAPS").is_none();
            ON.store(on as u8, Ordering::Relaxed);
            on
        }
    }
}

/// GC-safe `put` for the live interpreter path: grows + rehashes when dense, and if
/// the grow's table allocation overflows the nursery, runs a safepoint and
/// re-locates the (moved) map through its forwarding pointer before retrying.
/// `map` is updated in place to its post-collection location.
pub fn live_put(vm: &mut crate::Vm, map: &mut ArenaPtr, key: Value, val: Value) -> Value {
    // INSERT FIRST: open addressing keeps the load factor < 1, so a physical EMPTY
    // slot always exists. Writing key+value into the table NOW roots them through
    // the map (a tracked root), so the resize collection below cannot reclaim them —
    // no need to protect/relocate the key/value separately. `prev` (the returned
    // old value) IS in-flight and un-rooted, so it is protected across the grow.
    let mut prev = insert(vm, map, key, val);
    let used = read(*map, USED) as usize;
    let cap = read(*map, CAP) as usize;
    if used * 4 >= cap * 3 {
        let pi = protect(prev);
        live_grow(vm, map, cap * 2);
        prev = protected(pi);
        unprotect_to(pi);
    }
    prev
}

/// Grow to `new_cap` (a power of two) and rehash live entries. GC-safe: the table
/// allocation may overflow the nursery and collect, so the map is protected across
/// the collection and re-read as its forwarded pointer. Live entries ride along in
/// the (forwarded) old table, reachable through the protected map.
pub(crate) fn live_grow(vm: &mut crate::Vm, map: &mut ArenaPtr, new_cap: usize) {
    let base = temps_mark();
    let mi = protect(Value::Obj(*map));
    let new_table = loop {
        match alloc_table(new_cap) {
            Some(t) => break t,
            None => {
                vm.arena_collect_jit();
                *map = protected_map(mi);
            }
        }
    };
    let mut current = protected_map(mi);
    let old_table = table_of(current);
    let old_cap = read(current, CAP) as usize;
    let oi = protect(Value::Obj(old_table));
    write(current, TABLE, new_table.addr() as i64);
    write(current, CAP, new_cap as i64);
    write(current, SIZE, 0);
    write(current, USED, 0);
    for b in 0..old_cap {
        let old_table = protected_map(oi);
        if let Some((k, v)) = bucket_entry(old_table, b) {
            insert(vm, &mut current, k, v);
        }
    }
    *map = protected_map(mi);
    unprotect_to(base);
}
