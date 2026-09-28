//! Structural same-class static inlining. No runtime observations or heap access.
use cranelift::prelude::{types::I32, types::I64, InstBuilder, Value};
use cranelift::frontend::FunctionBuilder;
use rava_classfile::{ClassFile, Constant};

#[derive(Clone)]
pub struct NumericLeaf {
    args: usize,
    ops: Vec<Op>,
}

#[derive(Clone)]
enum Op { Arg(usize), Int(i32), Binary(u8), Neg, Call(Box<NumericLeaf>) }

impl NumericLeaf {
    pub(super) fn resolve(class: &ClassFile, cp: u16) -> Option<Self> {
        // One budget for the complete expansion, including repeated call sites.
        Self::resolve_inner(class, cp, &[], &mut 128)
    }

    fn resolve_inner(class: &ClassFile, cp: u16, ancestors: &[usize], remaining: &mut usize) -> Option<Self> {
        if ancestors.len() >= 8 { return None; }
        if !matches!(class.pool.get(cp)?, Constant::MethodRef { .. }) { return None; }
        let target = class.pool.member_ref(cp)?;
        if target.owner != class.this_class { return None; }
        let index = class.methods.iter().position(|m| m.name == target.name && m.descriptor == target.descriptor)?;
        if ancestors.contains(&index) { return None; }
        let mut path = ancestors.to_vec();
        path.push(index);
        let method = &class.methods[index];
        // static, non-synchronized, non-native, non-abstract; no initializer calls.
        if method.access_flags & 0x8 == 0 || method.access_flags & 0x520 != 0
            || method.name.starts_with('<') { return None; }
        let parameters = method.descriptor.strip_prefix('(')?.strip_suffix(")I")?;
        if !parameters.bytes().all(|b| b == b'I') || parameters.len() > 16 { return None; }
        let args = parameters.len();
        let code = method.code.as_ref()?;
        if !code.exceptions.is_empty() || code.bytecode.len() > 96
            || (code.max_locals as usize) < args { return None; }
        let bc = &code.bytecode;
        let mut ops = Vec::new();
        let (mut pc, mut depth) = (0, 0usize);
        while pc < bc.len() {
            *remaining = remaining.checked_sub(1)?;
            let op = bc[pc];
            pc += 1;
            let instruction = match op {
                0x02..=0x08 => Op::Int(op as i32 - 3),
                0x10 => { let v = *bc.get(pc)? as i8 as i32; pc += 1; Op::Int(v) }
                0x11 => { let v = i16::from_be_bytes([*bc.get(pc)?, *bc.get(pc + 1)?]); pc += 2; Op::Int(v as i32) }
                0x12 | 0x13 => {
                    let index = if op == 0x12 { let v = *bc.get(pc)? as u16; pc += 1; v }
                        else { let v = u16::from_be_bytes([*bc.get(pc)?, *bc.get(pc + 1)?]); pc += 2; v };
                    let Constant::Integer(v) = class.pool.get(index)? else { return None; };
                    Op::Int(*v)
                }
                0x15 => { let index = *bc.get(pc)? as usize; pc += 1; Op::Arg(index) }
                0x1a..=0x1d => Op::Arg((op - 0x1a) as usize),
                0x60 | 0x64 | 0x68 | 0x7e | 0x80 | 0x82 => {
                    if depth < 2 { return None; } depth -= 1; Op::Binary(op)
                }
                0x74 => { if depth == 0 { return None; } Op::Neg }
                0xb8 => {
                    let cp = u16::from_be_bytes([*bc.get(pc)?, *bc.get(pc + 1)?]);
                    pc += 2;
                    let callee = Self::resolve_inner(class, cp, &path, remaining)?;
                    if depth < callee.args { return None; }
                    depth = depth - callee.args + 1;
                    Op::Call(Box::new(callee))
                }
                0xac => return (pc == bc.len() && depth == 1).then_some(Self { args, ops }),
                _ => return None,
            };
            match instruction {
                Op::Arg(index) => { if index >= args { return None; } depth += 1; }
                Op::Int(_) => depth += 1,
                _ => {}
            }
            if depth > code.max_stack as usize { return None; }
            ops.push(instruction);
        }
        None
    }

    pub(super) fn emit(&self, b: &mut FunctionBuilder, args: &[Value]) -> Value {
        assert_eq!(args.len(), self.args);
        let args: Vec<_> = args.iter().map(|arg| b.ins().ireduce(I32, *arg)).collect();
        let result = self.emit_i32(b, &args);
        b.ins().sextend(I64, result)
    }

    fn emit_i32(&self, b: &mut FunctionBuilder, args: &[Value]) -> Value {
        let mut stack = Vec::new();
        for op in &self.ops {
            let value = match op {
                Op::Arg(index) => args[*index],
                Op::Int(value) => b.ins().iconst(I32, *value as i64),
                Op::Neg => { let value = stack.pop().unwrap(); b.ins().ineg(value) }
                Op::Call(callee) => {
                    let arguments = stack.split_off(stack.len() - callee.args);
                    callee.emit_i32(b, &arguments)
                }
                Op::Binary(op) => {
                    let right = stack.pop().unwrap();
                    let left = stack.pop().unwrap();
                    match op {
                        0x60 => b.ins().iadd(left, right),
                        0x64 => b.ins().isub(left, right),
                        0x68 => b.ins().imul(left, right),
                        0x7e => b.ins().band(left, right),
                        0x80 => b.ins().bor(left, right),
                        0x82 => b.ins().bxor(left, right),
                        _ => unreachable!(),
                    }
                }
            };
            stack.push(value);
        }
        stack[0]
    }
}
