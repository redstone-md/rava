//! Arena map tests.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::arena_map::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class_loader::ClassLoader;
    use crate::Vm;

    fn k(n: i32) -> Value {
        Value::Int(n)
    }

    #[test]
    fn gc_protected_temps_visited_and_scoped() {
        // The collector must visit every in-flight protected temporary (so it can
        // forward it), and the scope must pop cleanly.
        let base = temps_mark();
        let a = protect(Value::Int(7));
        let b = protect(Value::Obj(crate::nextgen::tlab_alloc_string(b"x").unwrap()));
        let mut seen = 0;
        forward_temps(&mut |v| {
            seen += 1;
            if let Value::Int(n) = v {
                *n += 100; // simulate the forwarding writeback
            }
        });
        assert_eq!(seen, temps_mark() - base, "every protected slot is visited");
        assert_eq!(protected(a).as_int(), Some(107), "forwarding writeback is visible");
        assert!(matches!(protected(b), Value::Obj(_)));
        unprotect_to(base);
        assert_eq!(temps_mark(), base, "scope pops to its mark");
    }

    #[test]
    fn protected_map_reads_the_forwarded_pointer() {
        let base = temps_mark();
        let old = crate::nextgen::tlab_alloc_string(b"old").unwrap();
        let moved = crate::nextgen::tlab_alloc_string(b"moved").unwrap();
        let mi = protect(Value::Obj(old));
        forward_temps(&mut |v| {
            if matches!(v, Value::Obj(p) if p.addr() == old.addr()) {
                *v = Value::Obj(moved);
            }
        });
        assert_eq!(protected_map(mi).addr(), moved.addr());
        unprotect_to(base);
    }

    #[test]
    fn put_get_update_remove_contains_basic() {
        let mut vm = Vm::new(ClassLoader::new());
        let mut m = new_map().unwrap();
        assert_eq!(size(m), 0);
        assert!(matches!(put(&mut vm, &mut m, k(1), k(10)), Some(Value::Null)));
        assert!(matches!(put(&mut vm, &mut m, k(2), k(20)), Some(Value::Null)));
        assert_eq!(get(&mut vm, &mut m, &k(1)).as_int(), Some(10));
        assert_eq!(get(&mut vm, &mut m, &k(2)).as_int(), Some(20));
        assert!(contains(&mut vm, &mut m, &k(1)));
        assert!(!contains(&mut vm, &mut m, &k(99)));
        assert!(matches!(get(&mut vm, &mut m, &k(99)), Value::Null));
        // update returns the previous value, size unchanged.
        assert_eq!(put(&mut vm, &mut m, k(1), k(11)).unwrap().as_int(), Some(10));
        assert_eq!(get(&mut vm, &mut m, &k(1)).as_int(), Some(11));
        assert_eq!(size(m), 2);
        // remove returns the value, then absent + probe still finds the neighbour.
        assert_eq!(remove(&mut vm, &mut m, &k(1)).as_int(), Some(11));
        assert!(!contains(&mut vm, &mut m, &k(1)));
        assert_eq!(get(&mut vm, &mut m, &k(2)).as_int(), Some(20));
        assert_eq!(size(m), 1);
    }

    #[test]
    fn forced_hash_collisions_probe_correctly() {
        // Keys whose spread hash lands in the same initial bucket must coexist via
        // probing. With cap=16 (mask 15), pick keys n and n+16: same `& mask`.
        let mut vm = Vm::new(ClassLoader::new());
        let mut m = new_map().unwrap();
        for step in 0..6 {
            let key = 3 + step * 16; // all hash to the same initial slot at cap 16
            put(&mut vm, &mut m, k(key), k(key * 100)).unwrap();
        }
        for step in 0..6 {
            let key = 3 + step * 16;
            assert_eq!(get(&mut vm, &mut m, &k(key)).as_int(), Some(key * 100), "key {key}");
        }
        // Remove a middle one; the chain past its tombstone must still resolve.
        assert!(remove(&mut vm, &mut m, &k(3 + 2 * 16)).as_int().is_some());
        for step in 0..6 {
            let key = 3 + step * 16;
            let want = if step == 2 { None } else { Some(key * 100) };
            assert_eq!(get(&mut vm, &mut m, &k(key)).as_int(), want, "after-remove key {key}");
        }
    }

    #[test]
    fn grows_and_rehashes_preserving_all_entries() {
        let mut vm = Vm::new(ClassLoader::new());
        let mut m = new_map().unwrap();
        let n = 500; // forces several doublings past the initial cap of 16
        for i in 0..n {
            assert!(matches!(put(&mut vm, &mut m, k(i), k(i * 7)), Some(Value::Null)));
        }
        assert_eq!(size(m), n);
        assert!(read(m, CAP) as usize >= 1024, "table doubled past the load factor");
        for i in 0..n {
            assert_eq!(get(&mut vm, &mut m, &k(i)).as_int(), Some(i * 7), "key {i} survived rehash");
        }
        assert!(!contains(&mut vm, &mut m, &k(n)));
    }

    #[test]
    fn null_key_and_null_value_distinct_from_absent() {
        let mut vm = Vm::new(ClassLoader::new());
        let mut m = new_map().unwrap();
        // A key mapped to a null value is PRESENT (contains true) yet get == null.
        put(&mut vm, &mut m, k(5), Value::Null).unwrap();
        assert!(contains(&mut vm, &mut m, &k(5)));
        assert!(matches!(get(&mut vm, &mut m, &k(5)), Value::Null));
        // The null KEY is a valid, distinct entry.
        put(&mut vm, &mut m, Value::Null, k(42)).unwrap();
        assert!(contains(&mut vm, &mut m, &Value::Null));
        assert_eq!(get(&mut vm, &mut m, &Value::Null).as_int(), Some(42));
        assert!(!contains(&mut vm, &mut m, &k(404)));
    }

    #[test]
    fn entries_survive_a_cheney_collection() {
        // GC-correctness: the map + its table + arena-OBJECT value cells are forwarded
        // by the moving collector (the table is ref-masked through the map's table
        // cell; value cells that encode a raw arena pointer are forwarded in place).
        // Keys are ints (MapKey::Int — reliable in the bare test VM, no rt.jar); the
        // VALUES are arena strings, exercising the pointer-forwarding path. (Arena
        // STRING *keys* match by content via java/lang/String value-equality, which a
        // production VM resolves but this minimal loader cannot — covered end-to-end
        // at the live seam, not here.)
        let mut vm = Vm::new(ClassLoader::new());
        let mut m = new_map().unwrap();
        for i in 0..40 {
            let val = Value::Obj(crate::nextgen::tlab_alloc_string(format!("val{i}").as_bytes()).unwrap());
            put(&mut vm, &mut m, k(i), val).unwrap();
        }
        assert_eq!(size(m), 40, "pre-GC size");
        let cap_pre = read(m, CAP);

        let mut roots: Vec<*mut u8> = vec![m.as_ptr()];
        let (_evac, _) = crate::nextgen::tlab_collect(&mut roots);
        let mut m2 = unsafe { ArenaPtr::new(roots[0]).unwrap() };

        assert_eq!(read(m2, CAP), cap_pre, "cap cell survived (got {})", read(m2, CAP));
        assert_eq!(size(m2), 40, "all entries survived the collection (got {})", size(m2));
        // Every int-keyed entry resolves, and its arena-string value's bytes are
        // intact — proof the collector forwarded the table's value pointers in place.
        for i in 0..40 {
            match get(&mut vm, &mut m2, &k(i)) {
                Value::Obj(p) => {
                    let bytes = crate::nextgen::arena_str_bytes(p);
                    assert_eq!(bytes, format!("val{i}").as_bytes(), "value {i} bytes post-GC");
                }
                other => panic!("key {i} lost its arena value post-GC: {other:?}"),
            }
        }
    }
}
