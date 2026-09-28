//! Insert and remove against the open-addressed table.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::arena_map::*;

/// Insert assuming the table has room (an EMPTY slot exists). Reuses the first
/// tombstone seen so deletions don't permanently bloat probe chains.
pub(crate) fn insert(vm: &mut crate::Vm, map: &mut ArenaPtr, key: Value, val: Value) -> Value {
    // Key derivation and stored-key comparison can execute guest `toString`, hence collect.
    // Keep all three values forwardable and never carry a table pointer across either call.
    let base = temps_mark();
    let mi = protect(Value::Obj(*map));
    let ki = protect(key);
    let vi = protect(val);
    let out = (|| {
        let key = protected(ki);
        let mk = crate::map::value_map_key(vm, &key);
        let h = hash_of(&mk);
        let current = protected_map(mi);
        let cap = read(current, CAP) as usize;
        let mask = cap - 1;
        let mut idx = (h as u32 as usize) & mask;
        let mut first_tomb: Option<usize> = None;
        for _ in 0..cap {
            // A prior `bucket_matches` may have collected. Re-derive the table from the
            // forwarded map at the top of every probe iteration.
            let current = protected_map(mi);
            let table = table_of(current);
            let head = read(table, idx * BW);
            match head_state(head) {
                EMPTY => {
                    let slot = first_tomb.unwrap_or(idx);
                    if first_tomb.is_none() {
                        write(current, USED, read(current, USED) + 1);
                    }
                    let key = protected(ki);
                    let val = protected(vi);
                    write_bucket(table, slot, h, &key, &mk, &val);
                    write(current, SIZE, read(current, SIZE) + 1);
                    return Value::Null;
                }
                TOMB => {
                    if first_tomb.is_none() {
                        first_tomb = Some(idx);
                    }
                }
                OCC if head_hash(head) == h && bucket_matches(vm, table, idx, head, &mk) => {
                    // The comparison itself can collect; discard `table` and re-read it.
                    let table = table_of(protected_map(mi));
                    let prev = decode_map_cell(read(table, idx * BW + 3));
                    let val = protected(vi);
                    write(table, idx * BW + 3, encode_map_cell(&val));
                    return prev;
                }
                _ => {}
            }
            idx = (idx + 1) & mask;
        }
        // Unreachable: `put` grows before the table can fill. Defensive no-op.
        Value::Null
    })();
    *map = protected_map(mi);
    unprotect_to(base);
    out
}

/// `map.remove(key)` — marks the bucket a tombstone, returns the removed value.
pub fn remove(vm: &mut crate::Vm, map: &mut ArenaPtr, key: &Value) -> Value {
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
        return Value::Null;
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
            EMPTY => return Value::Null,
            OCC if head_hash(head) == h && bucket_matches(vm, table, idx, head, &mk) => {
                let current = protected_map(mi);
                let table = table_of(current);
                let prev = decode_map_cell(read(table, idx * BW + 3));
                write(table, idx * BW, TOMB); // state TOMB, hash 0, INT_KEY 0
                write(table, idx * BW + 1, 0);
                write(table, idx * BW + 2, 0);
                write(table, idx * BW + 3, 0);
                write(current, SIZE, read(current, SIZE) - 1);
                // `used` stays (the tombstone still occupies a probe slot until
                // the next grow rehashes it away).
                return prev;
            }
            _ => {}
        }
        idx = (idx + 1) & mask;
      }
      Value::Null
    })();
    *map = protected_map(mi);
    unprotect_to(base);
    out
}
