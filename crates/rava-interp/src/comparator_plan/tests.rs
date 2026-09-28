//! Comparator plan tests.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::comparator_plan::*;

#[cfg(test)]
mod tests {
    use super::{directed, flatten, merge_sort_indices, KeyClause, KeyMemo};
    use crate::{ClassLoader, Lambda, LambdaSite, Value, Vm};
    use std::collections::HashSet;
    use std::sync::Arc;

    #[test]
    fn reversal_flips_normalized_comparison() {
        assert_eq!(directed(-1, false), -1);
        assert_eq!(directed(-1, true), 1);
        assert_eq!(directed(0, true), 0);
        assert_eq!(directed(1, true), -1);
    }

    fn key(vm: &mut Vm, reverse: bool) -> Value {
        let function = vm.heap.alloc("java/util/function/Function");
        let comparator = vm.heap.alloc("java/util/Comparator");
        vm.heap.put_field(
            comparator,
            "$cmp",
            Value::Str(if reverse { "keyrev" } else { "key" }.into()),
        );
        vm.heap
            .put_field(comparator, "$keyfn", Value::Ref(function));
        Value::Ref(comparator)
    }

    fn combine(vm: &mut Vm, kind: &str, first: Value, second: Option<Value>) -> Value {
        let comparator = vm.heap.alloc("java/util/Comparator");
        vm.heap
            .put_field(comparator, "$cmp", Value::Str(kind.into()));
        let field = if kind == "reversed" {
            "$inner"
        } else {
            "$first"
        };
        vm.heap.put_field(comparator, field, first);
        if let Some(second) = second {
            vm.heap.put_field(comparator, "$second", second);
        }
        Value::Ref(comparator)
    }

    fn flatten_for_test(vm: &Vm, comparator: &Value) -> Option<Vec<KeyClause>> {
        let mut clauses = Vec::new();
        flatten(
            vm,
            comparator,
            false,
            false,
            &mut HashSet::new(),
            &mut clauses,
        )?;
        Some(clauses)
    }

    fn ref_handle(value: &Value) -> u32 {
        match value {
            Value::Ref(handle) => *handle,
            _ => panic!("expected heap handle"),
        }
    }

    #[test]
    fn nested_then_and_reversed_preserve_clause_order() {
        let mut vm = Vm::new(ClassLoader::new());
        let first = key(&mut vm, false);
        let second_key = key(&mut vm, false);
        let first_function = vm.heap.get_field(ref_handle(&first), "$keyfn");
        let second_function = vm.heap.get_field(ref_handle(&second_key), "$keyfn");
        let second = combine(&mut vm, "reversed", second_key, None);
        let chain = combine(&mut vm, "then", first, Some(second));
        let clauses = flatten_for_test(&vm, &chain).expect("supported comparator tree");
        assert_eq!(clauses.len(), 2);
        assert_eq!(
            ref_handle(&clauses[0].function),
            ref_handle(&first_function)
        );
        assert_eq!(
            ref_handle(&clauses[1].function),
            ref_handle(&second_function)
        );
        assert!(!clauses[0].reverse);
        assert!(clauses[1].reverse);

        let whole = combine(&mut vm, "reversed", chain, None);
        let reversed = flatten_for_test(&vm, &whole).expect("supported reversed tree");
        assert!(reversed[0].reverse);
        assert!(!reversed[1].reverse);
    }

    #[test]
    fn cyclic_tree_falls_back() {
        let mut vm = Vm::new(ClassLoader::new());
        let comparator = vm.heap.alloc("java/util/Comparator");
        vm.heap
            .put_field(comparator, "$cmp", Value::Str("reversed".into()));
        vm.heap
            .put_field(comparator, "$inner", Value::Ref(comparator));
        assert!(flatten_for_test(&vm, &Value::Ref(comparator)).is_none());
    }

    #[test]
    fn lambda_leaf_can_snapshot_immutable_metadata() {
        let mut vm = Vm::new(ClassLoader::new());
        let function = vm.heap.alloc_lambda(Lambda {
            kind: 6,
            site: Arc::new(LambdaSite::new("example/Owner".into(), "key".into(), "(I)I".into())),
            captured: vec![Value::Int(7)],
        });
        let comparator = vm.heap.alloc("java/util/Comparator");
        vm.heap
            .put_field(comparator, "$cmp", Value::Str("key".into()));
        vm.heap
            .put_field(comparator, "$keyfn", Value::Ref(function));

        let mut clauses = Vec::new();
        flatten(
            &vm,
            &Value::Ref(comparator),
            false,
            true,
            &mut HashSet::new(),
            &mut clauses,
        )
        .expect("supported lambda comparator leaf");

        assert_eq!(clauses.len(), 1);
        let snapshot = clauses[0].lambda.as_ref().expect("lambda snapshot");
        assert_eq!(snapshot.kind, 6);
        assert_eq!(snapshot.owner(), "example/Owner");
        assert!(matches!(snapshot.captured.as_slice(), [Value::Int(7)]));
    }

    /// The permutation must be the STABLE ascending order for every size, including the
    /// odd-run cases (n not a power of two) where the merge widths do not divide evenly.
    #[test]
    fn index_merge_is_a_stable_ascending_sort() {
        for keys in [
            vec![],
            vec![7],
            vec![2, 1],
            vec![1, 2, 3],
            vec![3, 1, 2, 1, 5, 1, 4],
            vec![9, 8, 7, 6, 5, 4, 3, 2, 1],
            vec![0, 0, 0, 0, 0],
        ] {
            let order = merge_sort_indices(keys.len(), |a, b| Ok(keys[a].cmp(&keys[b]) as i32))
                .expect("infallible comparison");
            assert_eq!(order.len(), keys.len());
            let sorted: Vec<i32> = order.iter().map(|&i| keys[i]).collect();
            let mut expected = keys.clone();
            expected.sort();
            assert_eq!(sorted, expected, "wrong order for {keys:?}");
            // Stability: equal keys keep their input index order.
            for w in order.windows(2) {
                if keys[w[0]] == keys[w[1]] {
                    assert!(w[0] < w[1], "unstable for {keys:?}");
                }
            }
        }
    }

    /// A throwing extractor must abort the sort at the FIRST failing comparison, not run
    /// the rest of the schedule — that is what keeps exception timing the authoritative
    /// path's. `Vm::compare` propagates the same way.
    #[test]
    fn a_failing_comparison_aborts_the_merge() {
        let mut seen = 0usize;
        let result = merge_sort_indices(8, |_, _| {
            seen += 1;
            if seen == 3 {
                Err(crate::VmError::StackUnderflow)
            } else {
                Ok(-1)
            }
        });
        assert!(result.is_err());
        assert_eq!(seen, 3, "the merge kept comparing after an error");
    }

    /// The memo addresses one slot per `(element, clause)` — a collision here would serve
    /// one element's key for another's, which no gate hash would explain.
    #[test]
    fn memo_slots_are_distinct_per_element_and_clause() {
        let memo = KeyMemo { elements: 0, keys: 0, filled: vec![false; 4 * 3], tuple: 3 };
        let mut seen = HashSet::new();
        for element in 0..4 {
            for clause in 0..3 {
                let slot = memo.slot(element, clause);
                assert!(slot < memo.filled.len());
                assert!(seen.insert(slot), "slot {slot} reused");
            }
        }
        assert_eq!(seen.len(), 12);
    }
}
