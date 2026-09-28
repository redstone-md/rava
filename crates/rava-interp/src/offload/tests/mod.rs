//! Offload admission tests.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::offload::*;

#[cfg(test)]
use super::*;
use rava_classfile::{BootstrapMethod, Code, Constant};

fn code(bytecode: Vec<u8>) -> Code {
    Code { max_stack: 8, max_locals: 4, bytecode, exceptions: Vec::new(), ..Default::default() }
}
fn pool() -> ConstantPool {
    ConstantPool::from_entries(vec![Constant::Unusable])
}
fn yes(_: CalleeQuery) -> bool {
    true
}

// Pool whose #1 is a Fieldref Owner.f:I (so field_ref/field_width resolve).
fn pool_field_i() -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::FieldRef { class_index: 2, nat_index: 3 },
        Constant::Class { name_index: 4 },
        Constant::NameAndType { name_index: 5, desc_index: 6 },
        Constant::Utf8("Owner".into()),
        Constant::Utf8("f".into()),
        Constant::Utf8("I".into()),
    ])
}

fn pool_bitset_set() -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,                                     // 0
        Constant::Class { name_index: 8 },                     // 1 (for `new`)
        Constant::MethodRef { class_index: 3, nat_index: 4 },  // 2 (BitSet.set)
        Constant::Class { name_index: 5 },                     // 3
        Constant::NameAndType { name_index: 6, desc_index: 7 },// 4
        Constant::Utf8("java/util/BitSet".into()),             // 5
        Constant::Utf8("set".into()),                          // 6
        Constant::Utf8("(I)V".into()),                         // 7
        Constant::Utf8("java/util/BitSet".into()),             // 8
    ])
}

// Pool for the devirt tests: #1 Class Impl (the `new` site), #3 Impl.<init>()V,
// #7 Iface.m()V (declared on the interface Iface, impl on Impl).
fn pool_devirt() -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,                                     // 0
        Constant::Class { name_index: 2 },                     // 1 Impl (new site)
        Constant::Utf8("Impl".into()),                         // 2
        Constant::MethodRef { class_index: 1, nat_index: 4 },  // 3 Impl.<init>()V
        Constant::NameAndType { name_index: 5, desc_index: 6 },// 4
        Constant::Utf8("<init>".into()),                       // 5
        Constant::Utf8("()V".into()),                          // 6
        Constant::MethodRef { class_index: 8, nat_index: 10 }, // 7 Iface.m()V
        Constant::Class { name_index: 9 },                     // 8 Iface
        Constant::Utf8("Iface".into()),                        // 9
        Constant::NameAndType { name_index: 11, desc_index: 6 },// 10
        Constant::Utf8("m".into()),                            // 11
    ])
}

/// Pool with a concat `invokedynamic` at #1 (bootstrap 0 → MethodHandle #5 →
/// MethodRef #6 `bsm_owner.bsm_name`), call-site descriptor `site_desc`, plus
/// `Exc` (#12 Class, #14 `Exc.<init>(Ljava/lang/String;)V` MethodRef) for the
/// throw shape. Mirrors javac's `throw new Exc("..." + x)` pool layout.
fn pool_indy(bsm_owner: &str, bsm_name: &str, site_desc: &str) -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,                                      // 0
        Constant::InvokeDynamic { bootstrap: 0, nat_index: 2 },  // 1
        Constant::NameAndType { name_index: 3, desc_index: 4 },  // 2
        Constant::Utf8("concat".into()),                         // 3
        Constant::Utf8(site_desc.into()),                        // 4
        Constant::MethodHandle { ref_kind: 6, ref_index: 6 },    // 5 (invokeStatic)
        Constant::MethodRef { class_index: 7, nat_index: 8 },    // 6
        Constant::Class { name_index: 9 },                       // 7
        Constant::NameAndType { name_index: 10, desc_index: 11 },// 8
        Constant::Utf8(bsm_owner.into()),                        // 9
        Constant::Utf8(bsm_name.into()),                         // 10
        Constant::Utf8("(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/invoke/CallSite;".into()), // 11
        Constant::Class { name_index: 13 },                      // 12 Exc
        Constant::Utf8("Exc".into()),                            // 13
        Constant::MethodRef { class_index: 12, nat_index: 15 },  // 14 Exc.<init>(String)V
        Constant::NameAndType { name_index: 16, desc_index: 17 },// 15
        Constant::Utf8("<init>".into()),                         // 16
        Constant::Utf8("(Ljava/lang/String;)V".into()),          // 17
    ])
}
fn concat_pool(site_desc: &str) -> ConstantPool {
    pool_indy("java/lang/invoke/StringConcatFactory", "makeConcatWithConstants", site_desc)
}
fn bsm_table() -> Vec<BootstrapMethod> {
    vec![BootstrapMethod { handle_index: 5, args: Vec::new() }]
}

mod offload1;
mod offload2;
mod field_txn;
