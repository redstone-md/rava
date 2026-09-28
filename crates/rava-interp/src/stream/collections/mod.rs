//! Collection operations, iterators and Optional.
//!
//! Re-exported from the parent module, so no call site outside it changed.


pub(crate) mod coll_op;
pub(crate) use coll_op::*;
use crate::stream::*;

/// The synthetic iterator's live-source witness, shared by native and compiled calls.
pub(crate) fn iterator_source_changed(vm: &Vm, iterator: u32) -> bool {
    let Value::Int(expected) = vm.heap.get_field_syn(iterator, crate::heap::Syn::ExpSize) else {
        return false;
    };
    let Value::Ref(source) = vm.heap.get_field_syn(iterator, crate::heap::Syn::Source) else {
        return false;
    };
    coll_op::source_len(vm, source).is_some_and(|length| length as i32 != expected)
}

pub(crate) fn iter_op(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let h = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Ok(None),
    };
    let elems = elems_of(vm, args.first());
    let pos = vm.heap.get_field_syn(h, crate::heap::Syn::Pos).as_int().unwrap_or(0) as usize;
    let ret = match name {
        "hasNext" | "hasMoreElements" => Value::Int((pos < elems.len()) as i32),
        // next() and the primitive variants (fastutil/PrimitiveIterator) all
        // advance and return the element, coerced to the primitive type.
        "next" | "nextElement" | "nextInt" | "nextLong" | "nextFloat" | "nextDouble"
        | "nextShort" | "nextByte" | "nextChar" => {
            // Exhausted iterator: `next()` throws, it does not hand back null. A
            // caller that loops on something other than hasNext() (a counted loop, a
            // `while(true)` broken by the exception) read an endless run of nulls and
            // either NPE'd far from the cause or wrote nulls into a result.
            if pos >= elems.len() {
                return Err(VmError::Throw(
                    vm.make_exception("java/util/NoSuchElementException", ""),
                ));
            }
            // FAIL-FAST, exactly where the JDK puts it: in `next`, not in `hasNext`. A
            // one-element collection modified inside its own loop therefore does NOT throw
            // on either engine, because the iteration ends before the next check — which is
            // why bench/IterSem.java's `cme.map` line reads NO THROW on both.
            if iterator_source_changed(vm, h) {
                return Err(VmError::Throw(
                    vm.make_exception("java/util/ConcurrentModificationException", ""),
                ));
            }
            let v = elems.get(pos).cloned().unwrap_or(Value::Null);
            vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(pos as i32 + 1));
            match name {
                "nextInt" | "nextShort" | "nextByte" | "nextChar" => Value::Int(v.as_int().unwrap_or(0)),
                "nextLong" => Value::Long(v.as_long().unwrap_or(0)),
                "nextFloat" => Value::Float(v.as_f32()),
                "nextDouble" => Value::Double(v.as_f64()),
                _ => v,
            }
        }
        // Spliterator's own surface, over the same cursor.
        "estimateSize" | "getExactSizeIfKnown" => Value::Long((elems.len() - pos.min(elems.len())) as i64),
        // A cursor that declines to split is a legal spliterator; SIZED|ORDERED is what an
        // ordered snapshot honestly is.
        "trySplit" => Value::Null,
        "characteristics" => Value::Int(0x00000010 | 0x00000040), // ORDERED | SIZED
        "tryAdvance" => {
            if pos >= elems.len() {
                Value::Int(0)
            } else {
                let v = elems.get(pos).cloned().unwrap_or(Value::Null);
                vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(pos as i32 + 1));
                let f = args.get(1).cloned().unwrap_or(Value::Null);
                vm.apply(&f, vec![v])?;
                Value::Int(1)
            }
        }
        "hasPrevious" => Value::Int((pos > 0) as i32),
        // fastutil BidirectionalIterator.back(n): step back up to n elements,
        // returning how many were actually skipped. previousIndex()/nextIndex()
        // report the cursor. ThreadedLevelLightEngine walks a section list this way.
        "back" => {
            let n = args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let skipped = n.min(pos);
            vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int((pos - skipped) as i32));
            Value::Int(skipped as i32)
        }
        "previousIndex" => Value::Int(pos as i32 - 1),
        "nextIndex" => Value::Int(pos as i32),
        // previous()/previous*: step back one and return that element (primitive-coerced).
        "previous" | "previousInt" | "previousLong" | "previousShort" => {
            // Same fail-fast contract as `next` at the other end of the cursor.
            if pos == 0 {
                return Err(VmError::Throw(
                    vm.make_exception("java/util/NoSuchElementException", ""),
                ));
            }
            let np = pos - 1;
            let v = elems.get(np).cloned().unwrap_or(Value::Null);
            vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(np as i32));
            match name {
                "previousInt" | "previousShort" => Value::Int(v.as_int().unwrap_or(0)),
                "previousLong" => Value::Long(v.as_long().unwrap_or(0)),
                _ => v,
            }
        }
        // ListIterator.set / ListIterator.add WRITE THROUGH to the backing list.
        //
        // Neither had an arm, so both fell through to `coll_op` — which found the
        // ITERATOR's own `$elems` snapshot and dutifully modified that. `li.set("A")` and
        // `li.add("X")` therefore did nothing observable, silently
        // (bench/IterSem.java, `listIter.set` / `listIter.add`).
        "set" | "add" if matches!(vm.heap.get_field_syn(h, crate::heap::Syn::Source), Value::Ref(_)) => {
            let Value::Ref(src) = vm.heap.get_field_syn(h, crate::heap::Syn::Source) else { return Ok(None) };
            let kind = crate::immutable::kind_of(vm, h);
            let op = if name == "add" {
                crate::immutable::Op::Insert
            } else {
                crate::immutable::Op::Write
            };
            if kind != crate::immutable::NONE && crate::immutable::refuses(kind, op, true) {
                return Err(crate::immutable::uoe(vm));
            }
            let v = args.get(1).cloned().unwrap_or(Value::Null);
            let mut backing = vm.iterate_object(src)?;
            if name == "add" {
                // `add` inserts BEFORE the implicit cursor and advances it, so a following
                // `next()` returns what it would have returned before the insertion.
                let at = pos.min(backing.len());
                backing.insert(at, v);
                vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(pos as i32 + 1));
            } else {
                // `set` replaces the element `next()` last returned — the one BEFORE the
                // cursor — and is an IllegalStateException before any `next()`.
                if pos == 0 {
                    return Err(VmError::Throw(
                        vm.make_exception("java/lang/IllegalStateException", ""),
                    ));
                }
                backing[pos - 1] = v;
            }
            store_elems(vm, src, backing.clone());
            // A modification THROUGH the iterator is not a concurrent one — re-arm the
            // fail-fast witness, exactly as the JDK's `Itr` re-reads `modCount` after its
            // own `add`/`remove`. Without this the very next `next()` throws
            // ConcurrentModificationException at the caller's own edit.
            if let Value::Int(_) = vm.heap.get_field_syn(h, crate::heap::Syn::ExpSize) {
                vm.heap.put_field_syn(h, crate::heap::Syn::ExpSize, Value::Int(backing.len() as i32));
            }
            // The iterator's own snapshot has to follow, or the rest of the walk would
            // replay the pre-modification sequence.
            let arr = vm.heap.alloc_values(OBJ, backing);
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            return Ok(Some(None));
        }
        // forEachRemaining(Consumer): apply to each remaining element, advance.
        "forEachRemaining" => {
            let f = args.get(1).cloned().unwrap_or(Value::Null);
            let rest: Vec<Value> = elems.into_iter().skip(pos).collect();
            rooted_each(vm, rest, |vm, e| {
                vm.apply(&f, vec![e])?;
                Ok(std::ops::ControlFlow::Continue(()))
            })?;
            vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(i32::MAX));
            return Ok(Some(None));
        }
        // Iterator.remove(): delete the element last returned by next() from the
        // backing collection ($source). Without this, MC's advancement graph build
        // (a `while(!set.isEmpty()){ iterate; if resolved: it.remove() }` topo sort)
        // never shrank the set, looping O(n²) until the set ballooned to ~95k.
        "remove" => {
            // FAIL-FAST: `Iterator.remove` is legal exactly once per `next()`, and
            // never before the first one — the JDK (`Itr.lastRet < 0`) throws
            // IllegalStateException for both. `pos > 0` alone accepted a SECOND
            // remove (silently deleting the same element's predecessor again) and a
            // remove after a bare `hasNext` (a silent no-op).
            //
            // The state is derived from the CURSOR rather than stamped by `next`,
            // which keeps the 1.8M-calls-per-boot `next` fast path (natives/cascade.rs)
            // free of an extra field write: `$lastret` records the cursor value at
            // which a removal was already spent, so `$lastret == $pos` means "this
            // next() has been consumed" and `$pos == 0` means "no next() yet".
            // An iterator over an unmodifiable collection refuses `remove`, and it
            // refuses BEFORE the IllegalStateException bookkeeping: the JDK's
            // Itr.remove on an ImmutableCollections list throws UOE whatever the
            // cursor is doing.
            let kind = crate::immutable::kind_of(vm, h);
            if kind != crate::immutable::NONE
                && crate::immutable::refuses(kind, crate::immutable::Op::RemoveAlways, true)
            {
                return Err(crate::immutable::uoe(vm));
            }
            let spent = vm.heap.get_field(h, "$lastret").as_int().unwrap_or(0);
            if pos == 0 || spent == pos as i32 {
                return Err(VmError::Throw(
                    vm.make_exception("java/lang/IllegalStateException", ""),
                ));
            }
            vm.heap.put_field(h, "$lastret", Value::Int(pos as i32));
            {
                let target = elems.get(pos - 1).cloned().unwrap_or(Value::Null);
                if let Value::Ref(src) = vm.heap.get_field_syn(h, crate::heap::Syn::Source) {
                    // A keySet/entrySet view: delete the key from the BACKING map
                    // (MC's advancement build iterates map.entrySet() and removes
                    // resolved entries — without this the map never empties and the
                    // `while(!map.isEmpty())` topo sort ran O(n²) to ~95k elems).
                    // Shared with coll_op's remove/removeAll/retainAll/clear, so the
                    // three view kinds (key / value / entry) resolve to a backing-map
                    // key in exactly one place.
                    if let Some((owner, kind)) = crate::map::map_view_of(vm, src) {
                        crate::map::map_view_remove(vm, owner, kind, &target)?;
                    }
                    // Also drop from the source collection's own $elems.
                    let mut se = elems_of(vm, Some(&Value::Ref(src)));
                    let tmatch = |a: &Value, b: &Value| matches!((a, b), (Value::Ref(x), Value::Ref(y)) if x == y)
                        || a.display() == b.display();
                    if let Some(i) = se.iter().position(|e| tmatch(e, &target)) {
                        se.remove(i);
                        store_elems(vm, src, se);
                    }
                    // Re-arm the fail-fast witness: a removal THROUGH the iterator is the one
                    // structural change it is allowed to make, and the JDK's `Itr.remove`
                    // re-reads `modCount` for exactly this reason. Without it the next
                    // `next()` would throw ConcurrentModificationException at the caller's
                    // own legal edit — the `while (it.hasNext()) { if (..) it.remove(); }`
                    // shape MC's advancement topo sort is built on.
                    if let Value::Int(_) = vm.heap.get_field_syn(h, crate::heap::Syn::ExpSize) {
                        if let Some(n) = crate::stream::collections::coll_op::source_len(vm, src) {
                            vm.heap.put_field_syn(h, crate::heap::Syn::ExpSize, Value::Int(n as i32));
                        }
                    }
                }
            }
            return Ok(Some(None));
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

/// The `toString` prefix for an Optional flavour. `OptionalInt`/`OptionalLong`/
/// `OptionalDouble` print their OWN class name, and this one function is
/// dispatched for all four — hardcoding "Optional" mislabelled three of them.
fn optional_prefix(vm: &mut Vm, args: &[Value]) -> &'static str {
    // `value_class_name`, not `class_of`: an Optional receiver can be an ARENA
    // object, which has no heap handle for `class_of` to name — and all three
    // primitive flavours arrive that way, so the heap-only read mislabelled
    // exactly the ones this function exists for.
    let cls = match args.first() {
        Some(v) => vm.value_class_name(v),
        None => String::new(),
    };
    match cls.as_str() {
        "java/util/OptionalInt" => "OptionalInt",
        "java/util/OptionalLong" => "OptionalLong",
        "java/util/OptionalDouble" => "OptionalDouble",
        _ => "Optional",
    }
}

pub(crate) fn optional_op(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    // ARENA OPTIONAL: the receiver may be a `Value::Obj` whose `$val`/`$present` are real
    // cells. Matching `Value::Ref` alone would send every arena Optional to `Ok(None)` — the
    // silently-wrong-answer failure this migration exists to avoid, not a crash.
    #[cfg(feature = "next-gen-gc")]
    let arena_recv = match args.first() {
        Some(Value::Obj(p)) if crate::nextgen::arena_synth_enabled() => Some(Value::Obj(*p)),
        _ => None,
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let arena_recv: Option<Value> = None;
    let h = match args.first() {
        Some(Value::Ref(h)) => *h,
        // Sentinel: never dereferenced, because every read below goes through `arena_recv`
        // first and the four "return this optional" arms use `self_ref`.
        _ if arena_recv.is_some() => u32::MAX,
        _ => return Ok(None),
    };
    let self_ref = || match &arena_recv {
        Some(r) => r.clone(),
        None => Value::Ref(h),
    };
    #[cfg(feature = "next-gen-gc")]
    let (present, val) = match &arena_recv {
        Some(r) => (
            vm.synth_get(r, "$present").as_int().unwrap_or(0) != 0,
            vm.synth_get(r, "$val"),
        ),
        None => (
            vm.heap.get_field(h, "$present").as_int().unwrap_or(0) != 0,
            vm.heap.get_field_syn(h, crate::heap::Syn::Val),
        ),
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let (present, val) = (
        vm.heap.get_field(h, "$present").as_int().unwrap_or(0) != 0,
        vm.heap.get_field_syn(h, crate::heap::Syn::Val),
    );
    let ret = match name {
        "isPresent" => Value::Int(present as i32),
        "isEmpty" => Value::Int((!present) as i32),
        // TWO OPTIONALS ARE EQUAL WHEN THEIR CONTENTS ARE, and this fell through to
        // `Object.equals` — so `Optional.of("v").equals(Optional.of("v"))` was FALSE and
        // `Optional.empty().equals(Optional.empty())` was false too. Anything keying or
        // comparing on an Optional (a record component, a Set<Optional>) inherited that.
        "equals" => {
            let other = args.get(1).cloned().unwrap_or(Value::Null);
            let same = match &other {
                Value::Ref(oh) => {
                    let op = vm.heap.get_field(*oh, "$present").as_int().unwrap_or(0) != 0;
                    let ov = vm.heap.get_field_syn(*oh, crate::heap::Syn::Val);
                    if present != op {
                        false
                    } else if !present {
                        true
                    } else {
                        // The same element key every collection in this tree compares by,
                        // so `Optional` equality means what `Set` membership means.
                        crate::stream::ekey_mut(vm, &val) == crate::stream::ekey_mut(vm, &ov)
                    }
                }
                #[cfg(feature = "next-gen-gc")]
                Value::Obj(_) if crate::nextgen::arena_synth_enabled() => {
                    let op = vm.synth_get(&other, "$present").as_int().unwrap_or(0) != 0;
                    let ov = vm.synth_get(&other, "$val");
                    if present != op {
                        false
                    } else if !present {
                        true
                    } else {
                        // The same element key every collection in this tree compares by,
                        // so `Optional` equality means what `Set` membership means.
                        crate::stream::ekey_mut(vm, &val) == crate::stream::ekey_mut(vm, &ov)
                    }
                }
                _ => false,
            };
            Value::Int(same as i32)
        }
        // An empty Optional hashes to 0 and a present one to its value's hash, so the
        // equal ones above land in the same bucket.
        "hashCode" if !present => Value::Int(0),
        // `Optional.toString` is `Optional[value]` / `Optional.empty`, and guest
        // code prints it — logs, exception messages, assertion output. Without an
        // arm it fell through to `Object.toString` and rendered the handle
        // (`java.util.Optional@27`), which is both wrong and unstable across runs.
        "toString" => {
            let p = optional_prefix(vm, args);
            if present {
                let inner = vm.value_to_string(&val).unwrap_or_else(|_| val.display());
                Value::Str(format!("{p}[{inner}]").into())
            } else {
                Value::Str(format!("{p}.empty").into())
            }
        }
        "get" | "orElseThrow" | "getAsInt" | "getAsLong" | "getAsDouble" => {
            if present {
                val
            } else {
                // Empty Optional: get()/getAs*() throw NoSuchElementException;
                // orElseThrow(supplier) throws supplier.get(). Returning the
                // (null) $val silently masked a missing-registry composition gap
                // as a downstream `listElements` NPE (Wall #16) — honour JVM
                // semantics so the real cause surfaces. Gated to keep the
                // default build byte-identical.
                #[cfg(feature = "next-gen-gc")]
                {
                    if name == "orElseThrow" {
                        if let Some(f) = args.get(1).cloned() {
                            let ex = vm.apply(&f, vec![])?.unwrap_or(Value::Null);
                            if let Value::Ref(_) = ex {
                                return Err(VmError::Throw(ex));
                            }
                        }
                    }
                    return Err(VmError::Throw(
                        vm.make_exception("java/util/NoSuchElementException", "No value present"),
                    ));
                }
                #[cfg(not(feature = "next-gen-gc"))]
                {
                    val
                }
            }
        }
        "orElse" => if present { val } else { args.get(1).cloned().unwrap_or(Value::Null) },
        "orElseGet" => {
            if present { val }
            else { let f = args.get(1).cloned().unwrap_or(Value::Null); vm.apply(&f, vec![])?.unwrap_or(Value::Null) }
        }
        "map" | "flatMap" => {
            if present {
                let f = args.get(1).cloned().unwrap_or(Value::Null);
                let mapped = vm.apply(&f, vec![val])?.unwrap_or(Value::Null);
                // flatMap's function already returns an Optional.
                if name == "flatMap" { mapped } else { make_optional(vm, Some(mapped)) }
            } else {
                self_ref()
            }
        }
        "filter" => {
            if present {
                let f = args.get(1).cloned().unwrap_or(Value::Null);
                if truthy(&vm.apply(&f, vec![val])?) { self_ref() } else { make_optional(vm, None) }
            } else {
                self_ref()
            }
        }
        "ifPresent" => {
            if present {
                let f = args.get(1).cloned().unwrap_or(Value::Null);
                vm.apply(&f, vec![val])?;
            }
            return Ok(Some(None));
        }
        "ifPresentOrElse" => {
            let f = if present { args.get(1).cloned() } else { args.get(2).cloned() }.unwrap_or(Value::Null);
            let a = if present { vec![val] } else { vec![] };
            vm.apply(&f, a)?;
            return Ok(Some(None));
        }
        // stream(): a present Optional yields a single-element stream, empty an
        // empty one. Without this, Optional.stream() fell through to an empty
        // stream — so `xs.map(...).flatMap(Optional::stream)` dropped EVERYTHING
        // (e.g. TagLoader.loadTagsForExistingRegistries returned no tags).
        "stream" => make_stream(vm, if present { vec![val] } else { Vec::new() }),
        // or(supplier): keep this Optional if present, else the supplier's.
        "or" => {
            if present { self_ref() }
            else { let f = args.get(1).cloned().unwrap_or(Value::Null); vm.apply(&f, vec![])?.unwrap_or_else(|| make_optional(vm, None)) }
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
