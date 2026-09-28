//! Same-class, straight-line object leaves. No allocation or runtime admission history.
use super::*;
use rava_classfile::{ClassFile, Constant};

#[derive(Clone)]
pub struct ObjectLeaf {
    is_static: bool,
    is_private: bool,
    arguments: Vec<Shape>,
    receivers: Vec<usize>,
    ops: Vec<Op>,
}

#[derive(Clone, PartialEq)]
enum Shape { Int, Ref(String) }

#[derive(Clone)]
enum Op {
    Arg(usize), Dup,
    Field { name: String, owner: String, reference: bool, write: bool },
}

fn shape(text: &str) -> Option<Shape> {
    match text {
        "I" | "Z" | "B" | "C" | "S" => Some(Shape::Int),
        _ if text.starts_with('L') && text.ends_with(';') && text.len() > 2 =>
            Some(Shape::Ref(text[1..text.len() - 1].to_string())),
        _ => None,
    }
}

impl ObjectLeaf {
    pub(super) fn resolve(class: &ClassFile, cp: u16) -> Option<Self> {
        if !matches!(class.pool.get(cp)?, Constant::MethodRef { .. }) { return None; }
        let target = class.pool.member_ref(cp)?;
        if target.owner != class.this_class { return None; }
        let method = class.methods.iter().find(|m| m.name == target.name && m.descriptor == target.descriptor)?;
        let is_static = method.access_flags & 8 != 0;
        let is_private = method.access_flags & 2 != 0;
        if (!is_static && !is_private && method.access_flags & 0x10 == 0
            && class.access_flags & 0x10 == 0) || method.access_flags & 0x520 != 0
            || method.name.starts_with('<') { return None; }
        let (parameters, result) = method.descriptor.strip_prefix('(')?.split_once(')')?;
        let mut rest = parameters;
        let mut arguments = Vec::new();
        if !is_static { arguments.push(Shape::Ref(class.this_class.clone())); }
        while !rest.is_empty() {
            let size = if rest.starts_with('L') { rest.find(';')? + 1 } else { 1 };
            arguments.push(shape(rest.get(..size)?)?);
            rest = rest.get(size..)?;
        }
        if arguments.len() > 16 { return None; }
        let result = if result == "V" { None } else { Some(shape(result)?) };
        let code = method.code.as_ref()?;
        if !code.exceptions.is_empty() || code.bytecode.len() > 96
            || (code.max_locals as usize) < arguments.len() { return None; }
        // Origin is retained only for direct argument loads. Field results cannot
        // become receivers: that would require a late guard after possible writes.
        let mut stack: Vec<(Shape, Option<usize>)> = Vec::new();
        // Invocation itself checks this even if the body only uses another argument.
        let mut receivers = if is_static { Vec::new() } else { vec![0] };
        let mut ops = Vec::new();
        let mut pc = 0;
        while pc < code.bytecode.len() {
            let op = code.bytecode[pc];
            pc += 1;
            match op {
                0x15 | 0x19 | 0x1a..=0x1d | 0x2a..=0x2d => {
                    let index = match op {
                        0x15 | 0x19 => { let i = *code.bytecode.get(pc)? as usize; pc += 1; i },
                        0x1a..=0x1d => (op - 0x1a) as usize,
                        _ => (op - 0x2a) as usize,
                    };
                    let kind = arguments.get(index)?.clone();
                    let reference = matches!(kind, Shape::Ref(_));
                    if reference != matches!(op, 0x19 | 0x2a..=0x2d) { return None; }
                    stack.push((kind, Some(index)));
                    ops.push(Op::Arg(index));
                }
                0x59 => { stack.push(stack.last()?.clone()); ops.push(Op::Dup); }
                0xb4 | 0xb5 => {
                    let index = u16::from_be_bytes([*code.bytecode.get(pc)?, *code.bytecode.get(pc + 1)?]);
                    pc += 2;
                    if !matches!(class.pool.get(index)?, Constant::FieldRef { .. }) { return None; }
                    let field = class.pool.member_ref(index)?;
                    if field.owner != class.this_class { return None; }
                    let declaration = class.fields.iter().find(|f| f.name == field.name && f.descriptor == field.descriptor)?;
                    // Static, volatile and final writes need contracts outside this leaf.
                    if declaration.access_flags & 0x48 != 0
                        || (op == 0xb5 && declaration.access_flags & 0x10 != 0) { return None; }
                    let kind = shape(field.descriptor)?;
                    if op == 0xb5 && stack.pop()?.0 != kind { return None; }
                    let (receiver, origin) = stack.pop()?;
                    if receiver != Shape::Ref(class.this_class.to_string()) { return None; }
                    let origin = origin?;
                    if !receivers.contains(&origin) { receivers.push(origin); }
                    if op == 0xb4 { stack.push((kind.clone(), None)); }
                    ops.push(Op::Field { name: field.name.to_string(), owner: field.owner.to_string(),
                        reference: matches!(kind, Shape::Ref(_)), write: op == 0xb5 });
                }
                0xac | 0xb0 | 0xb1 => {
                    let valid = match &result {
                        None => op == 0xb1 && stack.is_empty(),
                        Some(kind) => stack.len() == 1 && &stack[0].0 == kind
                            && (op == 0xb0) == matches!(kind, Shape::Ref(_)) && op != 0xb1,
                    };
                    // Require an actual field operation; scalar/identity leaves are separate.
                    return (valid && pc == code.bytecode.len()
                        && ops.iter().any(|op| matches!(op, Op::Field { .. })))
                        .then_some(Self { is_static, is_private, arguments, receivers, ops });
                }
                _ => return None,
            }
            if stack.len() > code.max_stack as usize { return None; }
        }
        None
    }

    pub(super) fn accepts(&self, args: &[(Value, bool, bool)]) -> bool {
        args.len() == self.arguments.len() && args.iter().zip(&self.arguments)
            .all(|((_, reference, wide), kind)| !wide && *reference == matches!(kind, Shape::Ref(_)))
    }

    pub(super) fn supports_opcode(&self, opcode: u8) -> bool {
        match opcode {
            0xb8 => self.is_static,
            0xb6 => !self.is_static,
            0xb7 => !self.is_static && self.is_private,
            _ => false,
        }
    }

    pub(super) fn guard(&self, b: &mut FunctionBuilder, args: &[(Value, bool, bool)]) -> Value {
        let mut null = b.ins().icmp_imm(IntCC::Equal, args[self.receivers[0]].0, 0);
        for &index in &self.receivers[1..] {
            let next = b.ins().icmp_imm(IntCC::Equal, args[index].0, 0);
            null = b.ins().bor(null, next);
        }
        null
    }

    pub(super) fn emit(&self, b: &mut FunctionBuilder, args: &[(Value, bool, bool)],
        constants: Value, consts: &mut Vec<crate::cnp::ConstDesc>, cb: &Callbacks) -> Option<Value> {
        let mut stack = Vec::new();
        for op in &self.ops {
            match op {
                Op::Arg(index) => stack.push(args[*index].0),
                Op::Dup => stack.push(*stack.last().unwrap()),
                Op::Field { name, owner, reference, write } => {
                    let slot = intern_const(consts, crate::cnp::ConstDesc::FieldId(owner.clone(), name.clone()));
                    let fid = load_const(b, slot, constants);
                    if *write {
                        let value = stack.pop().unwrap();
                        let receiver = stack.pop().unwrap();
                        let kind = b.ins().iconst(I64, if *reference { 4 } else { 0 });
                        b.ins().call(cb.putfield, &[receiver, fid, value, kind]);
                    } else {
                        let receiver = stack.pop().unwrap();
                        let kind = b.ins().iconst(I64, i64::from(*reference));
                        let call = b.ins().call(cb.getfield, &[receiver, fid, kind]);
                        stack.push(b.inst_results(call)[0]);
                    }
                }
            }
        }
        stack.pop()
    }
}
