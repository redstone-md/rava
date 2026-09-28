//! JIT unit tests.
//!
//! Lifted verbatim out of the inline `mod tests` block; `super::*` still resolves to the
//! same place, so no test body changed.

use crate::value::{REF_TAG, Value as JVal};
use super::*;
use rava_classfile::{Code, Constant, ConstantPool};

fn code(bytecode: Vec<u8>, max_locals: u16) -> Code {
    Code {
        max_stack: 8,
        max_locals,
        bytecode,
        exceptions: Vec::new(),
     ..Default::default() }
}

fn empty_pool() -> ConstantPool {
    ConstantPool::from_entries(vec![Constant::Unusable])
}

/// λ-PIC target stub: records the buffer address it was called with and
/// returns the first cell — proving both the SHIFT (bufaddr+8) and that the
/// SAM args land as the callee's locals.
#[cfg(feature = "next-gen-gc")]
static LP_SEEN_BUF: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
#[cfg(feature = "next-gen-gc")]
extern "C" fn lp_target(buf: i64, _ctx: i64) -> i64 {
    LP_SEEN_BUF.store(buf, std::sync::atomic::Ordering::SeqCst);
    unsafe { *(buf as *const i64) }
}

fn no_native() -> std::collections::HashSet<u16> {
    std::collections::HashSet::new()
}

fn no_inline() -> std::collections::HashMap<u16, InlineCallee> {
    std::collections::HashMap::new()
}

fn no_statics() -> std::collections::HashMap<u16, (u32, JT)> {
    std::collections::HashMap::new()
}
#[cfg(feature = "next-gen-gc")]
fn no_new_arena() -> std::collections::HashMap<u16, (u32, u32)> {
    std::collections::HashMap::new()
}
#[cfg(feature = "next-gen-gc")]
fn no_ctor_inlines() -> std::collections::HashMap<u16, CtorInline> {
    std::collections::HashMap::new()
}
#[cfg(feature = "next-gen-gc")]
fn no_concat_shapes() -> std::collections::HashMap<u16, ConcatShape> {
    std::collections::HashMap::new()
}
#[cfg(feature = "next-gen-gc")]
fn no_hof_fusions(
) -> std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan> {
    std::collections::HashMap::new()
}
#[cfg(feature = "next-gen-gc")]
fn no_getter_inlines() -> std::collections::HashMap<u16, (u32, JT)> {
    std::collections::HashMap::new()
}

/// A pool holding one FieldRef `pkg/Holder.GRADIENT:[[I` at cp index 6 (the
/// SimplexNoise.GRADIENT shape that gradDot reads).
fn field_ref_pool() -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("pkg/Holder".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("GRADIENT".into()),
        Constant::Utf8("[[I".into()),
        Constant::NameAndType {
            name_index: 3,
            desc_index: 4,
        },
        Constant::FieldRef {
            class_index: 2,
            nat_index: 5,
        },
    ])
}

fn static_method_pool(name: &str, desc: &str) -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("pkg/MathFns".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8(name.into()),
        Constant::Utf8(desc.into()),
        Constant::NameAndType {
            name_index: 3,
            desc_index: 4,
        },
        Constant::MethodRef {
            class_index: 2,
            nat_index: 5,
        },
    ])
}

/// Pool for escape tests: Class#2 `X`, MethodRef#6 `X.<init>()V`, FieldRef#10
/// `X.fld:I`, MethodRef#14 `X.sink(LX;)V`.
#[cfg(feature = "next-gen-gc")]
fn escape_pool() -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("X".into()),                                    // 1
        Constant::Class { name_index: 1 },                             // 2
        Constant::Utf8("<init>".into()),                               // 3
        Constant::Utf8("()V".into()),                                  // 4
        Constant::NameAndType { name_index: 3, desc_index: 4 },        // 5
        Constant::MethodRef { class_index: 2, nat_index: 5 },          // 6
        Constant::Utf8("fld".into()),                                  // 7
        Constant::Utf8("I".into()),                                    // 8
        Constant::NameAndType { name_index: 7, desc_index: 8 },        // 9
        Constant::FieldRef { class_index: 2, nat_index: 9 },           // 10
        Constant::Utf8("sink".into()),                                 // 11
        Constant::Utf8("(LX;)V".into()),                               // 12
        Constant::NameAndType { name_index: 11, desc_index: 12 },      // 13
        Constant::MethodRef { class_index: 2, nat_index: 13 },         // 14
    ])
}

/// RAII setter for `crate::LOCKED_EXEC_TEST_OVERRIDE`, so a test states which half of the
/// GAP-S09 contract it is asserting and cannot leak that choice into the next test.
#[cfg(feature = "next-gen-gc")]
struct LockedExecMode;

#[cfg(feature = "next-gen-gc")]
impl LockedExecMode {
    fn force(on: bool) -> Self {
        crate::LOCKED_EXEC_TEST_OVERRIDE
            .store(if on { 2 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
        LockedExecMode
    }
}

#[cfg(feature = "next-gen-gc")]
impl Drop for LockedExecMode {
    fn drop(&mut self) {
        crate::LOCKED_EXEC_TEST_OVERRIDE.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A fresh low-address arena object of a one-field class with field kind `kind`.
#[cfg(feature = "next-gen-gc")]
fn arena_obj_one_field(kind: u8) -> crate::arena::ArenaPtr {
    use std::sync::Arc;
    let name: Arc<str> = Arc::from(format!("ArenaF{kind}").as_str());
    let layout: Arc<[(u32, u8)]> = vec![(0u32, kind)].into();
    let cid = crate::nextgen::register_class(&name, &layout);
    crate::nextgen::tlab_try_alloc(cid, 1).unwrap()
}

/// Constant pool with a single FieldRef `O.x : desc` at index 6.
#[cfg(feature = "next-gen-gc")]
fn field_pool(desc: &str) -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("O".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("x".into()),
        Constant::Utf8(desc.into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::FieldRef { class_index: 2, nat_index: 5 },
    ])
}

mod pic;
mod inline;
mod escape;
mod arena_fields;
mod arena_new;
mod analysis_dp;
mod analysis_match;
mod gilfree_worker;
mod backedge_poll;
mod gilfree_lock;
mod intrinsics;
mod arith;
mod arrays;
mod control;
mod dead_blocks;
