//! Structural proof and scalar ABI for guest tasks that may run outside the GIL.
//!
//! Admission is deliberately narrow and fail-closed. A supported zero-argument
//! SAM wrapper may only source exact primitive scalars from receiver fields, a
//! static LambdaMetafactory closure, or primitive fields snapshotted from its
//! reference captures, then call one already-compiled static primitive kernel
//! and one reference-returning post-adapter. The adapter and Future publication
//! still run under the GIL.

use crate::jit::{self, JitStatus};
use crate::{Lambda, Value, Vm, VmError};
use std::sync::Arc;

#[path = "detached_task_fields.rs"]
mod fields;
#[path = "detached_task_proof.rs"]
mod proof;
use fields::{RawField, ResolvedField};
use proof::{
    detachable_body, lambda_input_load, lambda_reference_argument_owner, lambda_wrapper_signature,
    post_result, post_signature, primitive_field_kind, primitive_signature, reference_field_owner,
    LambdaArgument, PrimitiveKind,
};
#[cfg(test)]
use proof::{primitive_load_len, reference_load_len};

#[derive(Clone)]
enum InputSource {
    Field(u32),
    LambdaCapture(usize),
    LambdaCaptureFields {
        capture: usize,
        fields: Arc<[ResolvedField]>,
    },
}

enum RawLambdaInput {
    Primitive {
        capture: usize,
        kind: PrimitiveKind,
    },
    Field {
        capture: usize,
        fields: Vec<RawField>,
        kind: PrimitiveKind,
    },
}

#[derive(Clone)]
struct ScalarInput {
    source: InputSource,
    kind: PrimitiveKind,
}

#[derive(Clone)]
struct MethodInvoke {
    owner: String,
    name: String,
    descriptor: String,
}

#[derive(Clone)]
struct PostInvoke {
    owner: String,
    name: String,
    descriptor: String,
}

#[derive(Clone)]
pub(crate) struct DetachedTaskPlan {
    entry: usize,
    frame_len: usize,
    result: PrimitiveKind,
    inputs: Vec<ScalarInput>,
    post: PostInvoke,
}

pub(crate) struct DetachedInvocation {
    plan: DetachedTaskPlan,
    cells: Vec<i64>,
}

impl DetachedTaskPlan {
    pub(crate) fn materialize(&self, vm: &mut Vm, task: &Value) -> Option<DetachedInvocation> {
        let mut cells = vec![0i64; self.frame_len];
        let mut slot = 0usize;
        let lambda = if self.inputs.iter().any(|input| {
            matches!(
                &input.source,
                InputSource::LambdaCapture(_) | InputSource::LambdaCaptureFields { .. }
            )
        }) {
            match task {
                Value::Ref(handle) => Some(vm.heap.lambda_of(*handle)?),
                _ => return None,
            }
        } else {
            None
        };
        for input in &self.inputs {
            let bits = match &input.source {
                InputSource::Field(id) => {
                    let value = read_instance_field(vm, task, *id)?;
                    input.kind.encode_field(value)?
                }
                InputSource::LambdaCapture(index) => {
                    let value = lambda.as_ref()?.captured.get(*index)?.clone();
                    input.kind.encode(value)?
                }
                InputSource::LambdaCaptureFields { capture, fields } => {
                    let mut value = lambda.as_ref()?.captured.get(*capture)?.clone();
                    for field in fields.iter() {
                        if vm.value_class_name(&value) != field.owner.as_ref() {
                            return None;
                        }
                        value = read_instance_field(vm, &value, field.id)?;
                    }
                    input.kind.encode_field(value)?
                }
            };
            if slot + input.kind.slots() > cells.len() {
                return None;
            }
            cells[slot] = bits;
            slot += input.kind.slots();
        }
        Some(DetachedInvocation {
            plan: self.clone(),
            cells,
        })
    }
}

fn read_instance_field(vm: &mut Vm, receiver: &Value, field: u32) -> Option<Value> {
    match receiver {
        Value::Ref(handle) => Some(vm.heap.get_field_hint(*handle, field, 0).0),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(pointer) => Some(crate::opcodes::arena_getfield(*pointer, field)),
        _ => None,
    }
}

impl DetachedInvocation {
    pub(crate) fn run(&mut self) -> i64 {
        // SAFETY: admission proves a primitive-only callback-free body, the code
        // pointer remains owned by the VM JIT cache, and cells cover its frame.
        unsafe { jit::call_raw(self.plan.entry as *const u8, &mut self.cells, 0) }
    }

    pub(crate) fn publish(self, vm: &mut Vm, bits: i64) -> Result<Option<Value>, VmError> {
        let argument = self.plan.result.decode(bits);
        let post = self.plan.post;
        match crate::natives::try_invoke(
            vm,
            &post.owner,
            &post.name,
            &post.descriptor,
            &[argument],
        )? {
            Some(result) => Ok(result),
            None => {
                let message = format!(
                    "invokestatic {}.{}{}",
                    post.owner, post.name, post.descriptor
                );
                Err(VmError::Throw(
                    vm.make_exception("java/lang/UnsatisfiedLinkError", &message),
                ))
            }
        }
    }
}

pub(crate) fn prepare(vm: &mut Vm, task: &Value) -> Option<DetachedTaskPlan> {
    if vm.trampoline {
        return None;
    }
    if let Value::Ref(handle) = task {
        if let Some(lambda) = vm.heap.lambda_of(*handle) {
            return prepare_lambda(vm, &lambda);
        }
    }
    prepare_sam(vm, task)
}

fn prepare_sam(vm: &mut Vm, task: &Value) -> Option<DetachedTaskPlan> {
    let task_class = vm.value_class_name(task);
    if task_class.is_empty() {
        return None;
    }
    let mut sam = None;
    for name in Vm::sam_candidates(0) {
        let resolved = vm.resolve_named(&task_class, name, 0).ok()?;
        if let Some((decl, descriptor, is_static)) = resolved {
            sam = Some((decl, *name, descriptor, is_static));
            break;
        }
    }
    let (wrapper_class, wrapper_name, wrapper_descriptor, wrapper_static) = sam?;
    if wrapper_static {
        return None;
    }
    let wrapper = wrapper_class
        .file
        .method(wrapper_name, &wrapper_descriptor)?;
    if wrapper.access_flags & (0x0008 | 0x0020 | 0x0100 | 0x0400) != 0 {
        return None;
    }
    let wrapper_code = wrapper.code.as_ref()?;
    if !wrapper_code.exceptions.is_empty() {
        return None;
    }

    let bytecode = &wrapper_code.bytecode;
    let pool = &wrapper_class.file.pool;
    let mut pc = 0usize;
    let mut raw_inputs = Vec::new();
    while bytecode.get(pc) == Some(&0x2a) && bytecode.get(pc + 1) == Some(&0xb4) {
        let index = u16::from_be_bytes([*bytecode.get(pc + 2)?, *bytecode.get(pc + 3)?]);
        let field = pool.member_ref(index)?;
        let kind = primitive_field_kind(field.descriptor)?;
        raw_inputs.push((
            field.owner.to_string(),
            field.name.to_string(),
            field.descriptor.to_string(),
            kind,
        ));
        pc += 4;
    }
    if bytecode.get(pc) != Some(&0xb8) {
        return None;
    }
    let target_index = u16::from_be_bytes([*bytecode.get(pc + 1)?, *bytecode.get(pc + 2)?]);
    let target_ref = pool.member_ref(target_index)?;
    let target = MethodInvoke {
        owner: target_ref.owner.to_string(),
        name: target_ref.name.to_string(),
        descriptor: target_ref.descriptor.to_string(),
    };
    pc += 3;

    if bytecode.get(pc) != Some(&0xb8) {
        return None;
    }
    let post_index = u16::from_be_bytes([*bytecode.get(pc + 1)?, *bytecode.get(pc + 2)?]);
    let post_ref = pool.member_ref(post_index)?;
    let post = PostInvoke {
        owner: post_ref.owner.to_string(),
        name: post_ref.name.to_string(),
        descriptor: post_ref.descriptor.to_string(),
    };
    pc += 3;
    if bytecode.get(pc) != Some(&0xb0) || pc + 1 != bytecode.len() {
        return None;
    }

    let mut inputs = Vec::with_capacity(raw_inputs.len());
    for (owner, name, descriptor, kind) in raw_inputs {
        inputs.push(ScalarInput {
            source: InputSource::Field(resolved_primitive_field_id(
                vm,
                &owner,
                &name,
                &descriptor,
            )?),
            kind,
        });
    }
    finish_plan(vm, target, post, inputs)
}

fn prepare_lambda(vm: &mut Vm, lambda: &Lambda) -> Option<DetachedTaskPlan> {
    if lambda.kind != 6 {
        return None;
    }
    let wrapper_class = vm
        .resolve_method(lambda.owner(), lambda.name(), lambda.descriptor())
        .ok()??;
    let wrapper = wrapper_class
        .file
        .method(lambda.name(), lambda.descriptor())?;
    if wrapper.access_flags & (0x0008 | 0x0020 | 0x0100 | 0x0400) != 0x0008 {
        return None;
    }
    let wrapper_code = wrapper.code.as_ref()?;
    if !wrapper_code.exceptions.is_empty() {
        return None;
    }
    let (arguments, wrapper_result) = lambda_wrapper_signature(lambda.descriptor())?;
    if arguments.len() != lambda.captured.len() {
        return None;
    }

    let bytecode = &wrapper_code.bytecode;
    let pool = &wrapper_class.file.pool;
    let mut pc = 0usize;
    let mut raw_inputs = Vec::new();
    while bytecode.get(pc) != Some(&0xb8) {
        let (capture, argument, load_length) = lambda_input_load(bytecode, pc, &arguments)?;
        pc += load_length;
        match argument {
            LambdaArgument::Primitive(kind) => {
                raw_inputs.push(RawLambdaInput::Primitive { capture, kind });
            }
            LambdaArgument::Reference => {
                let mut fields = Vec::new();
                let mut expected_owner =
                    lambda_reference_argument_owner(lambda.descriptor(), capture)?;
                loop {
                    if bytecode.get(pc) != Some(&0xb4) {
                        return None;
                    }
                    let field_index =
                        u16::from_be_bytes([*bytecode.get(pc + 1)?, *bytecode.get(pc + 2)?]);
                    let field = pool.member_ref(field_index)?;
                    if field.owner != expected_owner {
                        return None;
                    }
                    fields.push(RawField {
                        owner: field.owner.to_string(),
                        name: field.name.to_string(),
                        descriptor: field.descriptor.to_string(),
                    });
                    pc += 3;
                    if let Some(kind) = primitive_field_kind(field.descriptor) {
                        raw_inputs.push(RawLambdaInput::Field {
                            capture,
                            fields,
                            kind,
                        });
                        break;
                    }
                    expected_owner = reference_field_owner(field.descriptor)?;
                }
            }
        }
    }
    if bytecode.get(pc) != Some(&0xb8) {
        return None;
    }
    let target_index = u16::from_be_bytes([*bytecode.get(pc + 1)?, *bytecode.get(pc + 2)?]);
    let target_ref = pool.member_ref(target_index)?;
    let target = MethodInvoke {
        owner: target_ref.owner.to_string(),
        name: target_ref.name.to_string(),
        descriptor: target_ref.descriptor.to_string(),
    };
    pc += 3;

    if bytecode.get(pc) != Some(&0xb8) {
        return None;
    }
    let post_index = u16::from_be_bytes([*bytecode.get(pc + 1)?, *bytecode.get(pc + 2)?]);
    let post_ref = pool.member_ref(post_index)?;
    let post = PostInvoke {
        owner: post_ref.owner.to_string(),
        name: post_ref.name.to_string(),
        descriptor: post_ref.descriptor.to_string(),
    };
    pc += 3;
    if bytecode.get(pc) != Some(&0xb0) || pc + 1 != bytecode.len() {
        return None;
    }
    if post_result(&post.descriptor)? != wrapper_result {
        return None;
    }
    let mut inputs = Vec::with_capacity(raw_inputs.len());
    for input in raw_inputs {
        let input = match input {
            RawLambdaInput::Primitive { capture, kind } => ScalarInput {
                source: InputSource::LambdaCapture(capture),
                kind,
            },
            RawLambdaInput::Field {
                capture,
                fields,
                kind,
            } => {
                let fields = fields
                    .into_iter()
                    .map(|field| {
                        Some(ResolvedField {
                            id: resolved_instance_field_id(
                                vm,
                                &field.owner,
                                &field.name,
                                &field.descriptor,
                            )?,
                            owner: Arc::from(field.owner),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                ScalarInput {
                    source: InputSource::LambdaCaptureFields {
                        capture,
                        fields: Arc::from(fields),
                    },
                    kind,
                }
            }
        };
        inputs.push(input);
    }
    finish_plan(vm, target, post, inputs)
}

fn finish_plan(
    vm: &mut Vm,
    target: MethodInvoke,
    post: PostInvoke,
    inputs: Vec<ScalarInput>,
) -> Option<DetachedTaskPlan> {
    let (argument_kinds, result) = primitive_signature(&target.descriptor)?;
    if argument_kinds != inputs.iter().map(|input| input.kind).collect::<Vec<_>>() {
        return None;
    }
    if !post_signature(&post.descriptor, result) {
        return None;
    }
    if vm
        .resolve_method(&post.owner, &post.name, &post.descriptor)
        .ok()?
        .is_some()
    {
        return None;
    }
    let target_class = vm
        .resolve_method(&target.owner, &target.name, &target.descriptor)
        .ok()??;
    if !target_class.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let target_method = target_class.file.method(&target.name, &target.descriptor)?;
    if target_method.access_flags & (0x0008 | 0x0020 | 0x0100 | 0x0400) != 0x0008 {
        return None;
    }
    let target_code = target_method.code.as_ref()?;
    if !detachable_body(target_code, result) {
        return None;
    }
    let key = target_code as *const rava_classfile::Code as usize;
    let (entry, compiled_result, self_registering) = match vm.jit_cache.get(&key)? {
        JitStatus::Compiled(function) => (function.raw() as usize, function.ret, function.self_reg),
        _ => return None,
    };
    if compiled_result != Some(result.jit_type()) || self_registering {
        return None;
    }
    Some(DetachedTaskPlan {
        entry,
        frame_len: usize::from(target_code.max_locals + target_code.max_stack).max(1),
        result,
        inputs,
        post,
    })
}

fn resolved_primitive_field_id(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    descriptor: &str,
) -> Option<u32> {
    primitive_field_kind(descriptor)?;
    resolved_instance_field_id(vm, owner, name, descriptor)
}

fn resolved_instance_field_id(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    descriptor: &str,
) -> Option<u32> {
    let mut current = owner.to_string();
    for _ in 0..64 {
        let class = vm.loader.get_or_load(&current).ok()?;
        if let Some(field) = class
            .file
            .fields
            .iter()
            .find(|field| field.name == name && field.descriptor == descriptor)
        {
            if field.access_flags & 0x0008 != 0 {
                return None;
            }
            return Some(vm.resolve_field_id(owner, name));
        }
        current = class.file.super_class.as_ref()?.to_string();
    }
    None
}

#[cfg(test)]
#[path = "detached_task_tests.rs"]
mod tests;
