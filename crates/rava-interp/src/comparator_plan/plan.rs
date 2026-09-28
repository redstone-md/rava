//! The comparator plan and the key memo over it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::comparator_plan::*;

pub(crate) struct ComparatorPlan {
    clauses: Vec<KeyClause>,
}

impl ComparatorPlan {
    pub(crate) fn build(vm: &Vm, comparator: &Value) -> Option<Self> {
        // The stream dispatcher holds native_depth>0, so maybe_gc cannot move
        // arena values during comparator callbacks. Missing that GC invariant
        // must use the authoritative recursive path.
        if !enabled() || crate::frames::native_depth() == 0 {
            return None;
        }
        let mut clauses = Vec::new();
        let mut visiting = HashSet::new();
        flatten(
            vm,
            comparator,
            false,
            lambda_snapshot_enabled(),
            &mut visiting,
            &mut clauses,
        )?;
        // The existing leaf-key cache is already cheaper; this plan targets
        // only composite comparator trees.
        let built = (clauses.len() > 1).then_some(Self { clauses });
        if built.is_some() && cmp_plan_census_enabled() {
            PLANS_BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        built
    }

    /// Compare with the exact recursive `Vm::compare` semantics. Admission has
    /// proved `native_depth>0`, which blocks moving GC across both applies.
    pub(crate) fn compare(&self, vm: &mut Vm, left: Value, right: Value) -> Result<i32, VmError> {
        debug_assert!(crate::frames::native_depth() > 0);
        for clause in &self.clauses {
            let left_key = apply_clause(vm, clause, left.clone())?.unwrap_or(Value::Null);
            let right_key = apply_clause(vm, clause, right.clone())?.unwrap_or(Value::Null);
            let ordering = crate::natural_cmp(&left_key, &right_key);
            if ordering != 0 {
                return Ok(directed(ordering, clause.reverse));
            }
        }
        Ok(0)
    }

    /// Number of flattened key clauses (the width of one element's key tuple).
    pub(crate) fn clause_count(&self) -> usize {
        self.clauses.len()
    }

    /// KEY-CACHE (sort fast path): extract `element`'s full key tuple — one apply per
    /// clause, in clause order. The merge sort otherwise re-runs each clause's extractor on
    /// every comparison (O(n log n) applies); caching each element's tuple ONCE makes it
    /// O(n). Applies every clause (no short-circuit), so it is only sound when the extractors
    /// are pure — the same assumption the single-key fast path already relies on. `native_depth>0`
    /// blocks moving GC across the applies (build admission).
    pub(crate) fn extract_keys(&self, vm: &mut Vm, element: Value) -> Result<Vec<Value>, VmError> {
        let mut keys = Vec::with_capacity(self.clauses.len());
        for clause in &self.clauses {
            keys.push(apply_clause(vm, clause, element.clone())?.unwrap_or(Value::Null));
        }
        Ok(keys)
    }

    /// KEY-CACHE: compare two pre-extracted key tuples with the exact clause order,
    /// short-circuit and reverse semantics of `compare` — but NO guest re-entry.
    pub(crate) fn compare_keys(&self, a: &[Value], b: &[Value]) -> i32 {
        for (i, clause) in self.clauses.iter().enumerate() {
            let ordering = crate::natural_cmp(&a[i], &b[i]);
            if ordering != 0 {
                return directed(ordering, clause.reverse);
            }
        }
        0
    }

    /// LAZY KEY MEMO — sort `elems` under this plan, extracting each `(element, clause)`
    /// key AT MOST ONCE and only when a comparison actually REACHES that clause.
    ///
    /// WHAT THIS FIXES. `extract_keys` builds every element's WHOLE tuple up front, so a
    /// plan of `c` clauses runs `n × c` extractors. The authoritative `compare` reaches
    /// clause `k` only after clauses `0..k` all tied, and on a first clause that almost
    /// always decides that is `n` extractions, not `n × c`. The gap is the whole cost: a
    /// datapack load sorts 213,891 elements through six-clause `thenComparing` trees, and
    /// the eager cache bills 1,497,237 extractor applies for it — exactly `213,891 × 7`,
    /// of which ~213,891 are what the semantics ask for.
    ///
    /// WHAT IT PRESERVES. The merge schedule below is the authoritative one (the same
    /// bottom-up stable merge the `compare` path runs), so the comparisons are the same
    /// comparisons, taken in the same order, and each clause is still consulted only after
    /// the earlier ones tied. The memo therefore performs EXACTLY the extraction set of
    /// the `compare` path, in the same first-touch order — with the repeats removed. Same
    /// short-circuit points, same first exception, same clause order and reversal. Against
    /// the eager cache it is strictly closer to the authoritative path, not further.
    ///
    /// GC: every element and every extracted key lives in a HEAP `Object[]` (whose cells
    /// the collector forwards in place) across the extractor callbacks; only `usize`
    /// indices and fill flags are held in Rust. Admission has proved `native_depth > 0`.
    pub(crate) fn sort_memoised(&self, vm: &mut Vm, elems: &mut [Value]) -> Result<(), VmError> {
        debug_assert!(crate::frames::native_depth() > 0);
        let n = elems.len();
        if n < 2 {
            return Ok(());
        }
        let width_of_tuple = self.clauses.len();
        let mut memo = KeyMemo {
            elements: vm.heap.alloc_values(crate::stream::OBJ, elems.to_vec()),
            keys: vm
                .heap
                .alloc_values(crate::stream::OBJ, vec![Value::Null; n * width_of_tuple]),
            filled: vec![false; n * width_of_tuple],
            tuple: width_of_tuple,
        };
        let order = merge_sort_indices(n, |left, right| {
            self.compare_memoised(vm, &mut memo, left, right)
        })?;
        // Read the elements back through the (forwarded) rooted array, never through the
        // caller's stale Rust copies.
        let elements = memo.elements;
        for (slot, &i) in elems.iter_mut().zip(order.iter()) {
            *slot = vm.heap.array_get(elements, i).unwrap_or(Value::Null);
        }
        Ok(())
    }

    /// `compare` over two element INDICES, taking each clause key from the memo. Identical
    /// clause order, short-circuit and reversal; the only difference is that a key already
    /// extracted for an earlier comparison is not extracted again.
    fn compare_memoised(
        &self,
        vm: &mut Vm,
        memo: &mut KeyMemo,
        left: usize,
        right: usize,
    ) -> Result<i32, VmError> {
        for (index, clause) in self.clauses.iter().enumerate() {
            // Left then right, the order `compare` applies them in.
            memo.ensure(vm, clause, left, index)?;
            memo.ensure(vm, clause, right, index)?;
            // Both keys are re-read AFTER the last apply: an extractor can run a moving
            // collection, and the rooted array is what forwards.
            let ordering = crate::natural_cmp(&memo.key(vm, left, index), &memo.key(vm, right, index));
            if ordering != 0 {
                return Ok(directed(ordering, clause.reverse));
            }
        }
        Ok(0)
    }
}

/// One sort's lazily-filled key table. `elements` and `keys` are heap `Object[]` handles
/// (stable across collections, cells forwarded in place); `filled` marks which
/// `(element, clause)` slots an extractor has already produced.
pub(crate) struct KeyMemo {
    pub(crate) elements: u32,
    pub(crate) keys: u32,
    pub(crate) filled: Vec<bool>,
    pub(crate) tuple: usize,
}

impl KeyMemo {
    #[inline]
    pub(crate) fn slot(&self, element: usize, clause: usize) -> usize {
        element * self.tuple + clause
    }

    /// Extract `(element, clause)`'s key on its FIRST touch only. A repeat touch runs no
    /// guest code — that is the whole saving.
    fn ensure(
        &mut self,
        vm: &mut Vm,
        clause: &KeyClause,
        element: usize,
        index: usize,
    ) -> Result<(), VmError> {
        let slot = self.slot(element, index);
        if self.filled[slot] {
            return Ok(());
        }
        let value = vm.heap.array_get(self.elements, element).unwrap_or(Value::Null);
        let key = apply_clause(vm, clause, value)?.unwrap_or(Value::Null);
        vm.heap.array_set(self.keys, slot, key);
        self.filled[slot] = true;
        Ok(())
    }

    #[inline]
    fn key(&self, vm: &Vm, element: usize, index: usize) -> Value {
        vm.heap
            .array_get(self.keys, self.slot(element, index))
            .unwrap_or(Value::Null)
    }
}

pub(crate) fn flatten(
    vm: &Vm,
    comparator: &Value,
    reverse: bool,
    snapshot_lambdas: bool,
    visiting: &mut HashSet<u32>,
    clauses: &mut Vec<KeyClause>,
) -> Option<()> {
    let Value::Ref(handle) = comparator else {
        return None;
    };
    if clauses.len() >= MAX_CLAUSES || !visiting.insert(*handle) {
        return None;
    }
    if vm.heap.lambda_of(*handle).is_some() {
        visiting.remove(handle);
        return None;
    }
    let kind = match vm.heap.get_field(*handle, "$cmp") {
        Value::Str(kind) => kind,
        _ => {
            visiting.remove(handle);
            return None;
        }
    };
    let result = match kind.as_ref() {
        "key" | "keyrev" => {
            let function = vm.heap.get_field(*handle, "$keyfn");
            // Heap handles are stable across moving arena collections. Other
            // representations would need a separate root in the plan.
            //
            // COST OF THAT REFUSAL (`RAVA_CMP_PLAN_CENSUS`, inert otherwise). Under
            // RAVA_ARENA_LAMBDA the key function IS an arena object, so every keyed
            // comparator is rejected here and `List.sort` falls back to calling the guest
            // comparator per comparison. RIP sampling of a full boot puts a number on it:
            // `java/util/List#sort` goes from 30 ticks (2.8% of native bridges) to 327
            // (24.2%, the #1 bridge) — a 10.9x jump that accounts for MORE than the whole
            // +264-tick bridge regression. Counted, not inferred, before anything is built.
            //
            // An ARENA key function is admitted too. The original refusal reasoned that a
            // moving collection could relocate it, but `build` above admits a plan ONLY
            // when `native_depth() > 0`, and under that condition `maybe_gc` cannot move
            // arena values for the whole life of the plan — the same invariant the
            // key-cache's own purity/GC argument already rests on. So an arena pointer here
            // is exactly as stable as a handle, and refusing it bought nothing but the slow
            // path. `apply_clause` routes a non-snapshot clause through `vm.apply`, which
            // takes any representation.
            let admissible = matches!(function, Value::Ref(_))
                || (cfg!(feature = "next-gen-gc") && matches!(function, Value::Obj(_)));
            if !admissible {
                if cmp_plan_census_enabled() {
                    KEYFN_NOT_REF.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                None
            } else {
                clauses.push(KeyClause {
                    lambda: if snapshot_lambdas {
                        match &function {
                            Value::Ref(handle) => vm.heap.lambda_of(*handle),
                            _ => None,
                        }
                    } else {
                        None
                    },
                    function,
                    reverse: reverse ^ (kind.as_ref() == "keyrev"),
                });
                Some(())
            }
        }
        "then" => {
            let first = vm.heap.get_field(*handle, "$first");
            let second = vm.heap.get_field(*handle, "$second");
            flatten(vm, &first, reverse, snapshot_lambdas, visiting, clauses)
                .and_then(|()| flatten(vm, &second, reverse, snapshot_lambdas, visiting, clauses))
        }
        "reversed" => {
            let inner = vm.heap.get_field(*handle, "$inner");
            flatten(vm, &inner, !reverse, snapshot_lambdas, visiting, clauses)
        }
        _ => None,
    };
    visiting.remove(handle);
    result
}
