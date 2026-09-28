//! Compile policy for cold, structurally proven data-parallel methods.
//!
//! The normal JIT waits for an invocation threshold. That is the wrong latency
//! policy for a large one-shot primitive loop: interpreting its first invocation
//! serially dominates the work and prevents the already-proven parallel codegen
//! from helping cold execution. Eligibility here is deliberately narrow and is
//! derived only from bytecode/CFG/dataflow facts.

use rava_classfile::Code;

/// Pure class-load predicate for bypassing the ordinary hotness threshold. Runtime feature
/// switches are intentionally absent: they may disable execution but cannot change this fact.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn structural_candidate(code: &Code, descriptor: &str, has_back_edge: bool) -> bool {
    let bytecode = &code.bytecode;
    if !code.exceptions.is_empty() || !has_back_edge {
        return false;
    }

    let primitive_reduction =
        crate::jit::analyze_data_parallel_loop(bytecode, crate::jit::ret_jt(descriptor))
            .is_some_and(|plan| {
                // A constant-pool accumulator seed is resolved and type-checked by the
                // normal compiler while the driver still owns the GIL.
                plan.step == 1 && plan.fold_static_cp.is_none()
            });
    if primitive_reduction {
        return true;
    }

    descriptor.ends_with(")V")
        && crate::jit::analyze_data_parallel_map(bytecode).is_some_and(|plan| {
            // ldc/ldc2 constants are resolved exactly under the GIL. Calls stay
            // excluded from this cold policy even though the mature hot policy
            // can validate selected pure intrinsics.
            plan.step == 1 && plan.static_calls.is_empty()
        })
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod tests {
    use super::*;

    fn code(bytecode: Vec<u8>) -> Code {
        Code {
            max_stack: 8,
            max_locals: 8,
            bytecode,
            exceptions: Vec::new(),
         ..Default::default() }
    }

    #[test]
    fn admits_call_free_disjoint_primitive_map() {
        // void map(long[] out, long[] input, int n) {
        //   for (int i=0; i<n; i++) out[i] = input[i] * input[i];
        // }
        let candidate = code(vec![
            0x03, 0x3e, // iconst_0; istore_3
            0x1d, 0x1c, 0xa2, 0x00, 0x13, // if i >= n, exit
            0x2a, 0x1d, // out, i
            0x2b, 0x1d, 0x2f, // input[i]
            0x2b, 0x1d, 0x2f, // input[i]
            0x69, 0x50, // lmul; lastore
            0x84, 0x03, 0x01, // iinc 3, 1
            0xa7, 0xff, 0xee, // goto condition
            0xb1,
        ]);

        assert!(structural_candidate(&candidate, "([J[JI)V", true));
    }

    #[test]
    fn admits_call_free_associative_reduction() {
        let candidate = code(vec![
            0x09, 0x40, 0x03, 0x3e, // long sum=0; int i=0
            0x1d, 0x1a, 0xa2, 0x00, 0x15, // if i >= n, exit
            0x1f, // sum
            0x1d, 0x10, 0x1f, 0x68, 0x1d, 0x06, 0x7a, 0x82, // pure value
            0x85, 0x61, 0x40, // i2l; ladd; store sum
            0x84, 0x03, 0x01, 0xa7, 0xff, 0xec, 0x1f, 0xad,
        ]);

        assert!(structural_candidate(&candidate, "(I)J", true));
    }

    #[test]
    fn rejects_shared_store_and_non_void_map_shape() {
        let shared_store = code(vec![0x03, 0xb3, 0x00, 0x01, 0xb1]);
        assert!(!structural_candidate(&shared_store, "()V", true));
        assert!(!structural_candidate(&shared_store, "()I", true));
        assert!(!structural_candidate(&shared_store, "()V", false));
    }
}
