//! The bytecode dispatch loop.

use super::support::{self as s, Frame};
use crate::classfile::CodeInfo;
use crate::heap::{obj, Oop};
use crate::method::Method;
use crate::runtime::throw_new;
use crate::thread::{JResult, Thread};

macro_rules! i32_op { ($f:ident, $op:expr) => {{ let b = $f.pop_i(); let a = $f.pop_i(); $f.push_i($op(a, b)); }} }
macro_rules! i64_op { ($f:ident, $op:expr) => {{ let b = $f.pop_l(); let a = $f.pop_l(); $f.push_l($op(a, b)); }} }
macro_rules! f32_op { ($f:ident, $op:expr) => {{ let b = $f.pop_f(); let a = $f.pop_f(); $f.push_f($op(a, b)); }} }
macro_rules! f64_op { ($f:ident, $op:expr) => {{ let b = $f.pop_d(); let a = $f.pop_d(); $f.push_d($op(a, b)); }} }

pub(super) fn execute(t: &mut Thread, m: &'static Method, code: &'static CodeInfo, locals: *mut u64) -> JResult<u64> {
    let mut f = Frame::new(m, code, locals);
    if let Some(top) = t.frames.last_mut() {
        top.1 = &f.op_pc;
    }
    loop {
        match step(t, &mut f) {
            Ok(Some(ret)) => return Ok(ret),
            Ok(None) => {}
            Err(ex) => match s::find_handler(t, &f, ex.0) {
                Some(handler) => {
                    f.sp = f.stack_base;
                    f.push_ref(ex.0);
                    f.pc = handler;
                }
                None => return Err(ex),
            },
        }
    }
}

/// Runs bytecodes until a return (`Some`) or an exception (`Err`). Returning `Ok(None)` after an
/// exception handler has been installed is never needed: handlers are found by `execute`.
#[inline(never)]
fn step(t: &mut Thread, f: &mut Frame) -> JResult<Option<u64>> {
    loop {
        let op = f.code[f.pc];
        f.op_pc = f.pc;
        match op {
            0x00 => f.pc += 1,
            0x01 => { f.push_ref(std::ptr::null_mut()); f.pc += 1 }
            0x02..=0x08 => { f.push_i(op as i32 - 3); f.pc += 1 }
            0x09 | 0x0a => { f.push_l((op - 0x09) as i64); f.pc += 1 }
            0x0b..=0x0d => { f.push_f((op - 0x0b) as f32); f.pc += 1 }
            0x0e | 0x0f => { f.push_d((op - 0x0e) as f64); f.pc += 1 }
            0x10 => { f.push_i(f.code[f.pc + 1] as i8 as i32); f.pc += 2 }
            0x11 => { f.push_i(f.u2(f.pc + 1) as i16 as i32); f.pc += 3 }
            0x12 => { let i = f.code[f.pc + 1] as u16; s::ldc(t, f, i)?; f.pc += 2 }
            0x13 | 0x14 => { let i = f.u2(f.pc + 1); s::ldc(t, f, i)?; f.pc += 3 }
            // loads
            0x15 | 0x17 | 0x19 => { let i = f.code[f.pc + 1] as usize; f.push(f.local(i)); f.pc += 2 }
            0x16 | 0x18 => { let i = f.code[f.pc + 1] as usize; f.push(f.local(i)); f.push(0); f.pc += 2 }
            0x1a..=0x1d => { f.push(f.local((op - 0x1a) as usize)); f.pc += 1 }
            0x1e..=0x21 => { f.push(f.local((op - 0x1e) as usize)); f.push(0); f.pc += 1 }
            0x22..=0x25 => { f.push(f.local((op - 0x22) as usize)); f.pc += 1 }
            0x26..=0x29 => { f.push(f.local((op - 0x26) as usize)); f.push(0); f.pc += 1 }
            0x2a..=0x2d => { f.push(f.local((op - 0x2a) as usize)); f.pc += 1 }
            // array loads
            0x2e => { let (a, i) = s::array_index(t, f)?; f.push_i(unsafe { *obj::array_data::<i32>(a).add(i) }) ; f.pc += 1 }
            0x2f => { let (a, i) = s::array_index(t, f)?; f.push_l(unsafe { *obj::array_data::<i64>(a).add(i) }); f.pc += 1 }
            0x30 => { let (a, i) = s::array_index(t, f)?; f.push(unsafe { *obj::array_data::<u32>(a).add(i) } as u64); f.pc += 1 }
            0x31 => { let (a, i) = s::array_index(t, f)?; let v = unsafe { *obj::array_data::<u64>(a).add(i) }; f.push(v); f.push(0); f.pc += 1 }
            0x32 => { let (a, i) = s::array_index(t, f)?; f.push(unsafe { *obj::array_data::<u64>(a).add(i) }); f.pc += 1 }
            0x33 => { let (a, i) = s::array_index(t, f)?; f.push_i(unsafe { *obj::array_data::<i8>(a).add(i) } as i32); f.pc += 1 }
            0x34 => { let (a, i) = s::array_index(t, f)?; f.push_i(unsafe { *obj::array_data::<u16>(a).add(i) } as i32); f.pc += 1 }
            0x35 => { let (a, i) = s::array_index(t, f)?; f.push_i(unsafe { *obj::array_data::<i16>(a).add(i) } as i32); f.pc += 1 }
            // stores
            0x36 | 0x38 | 0x3a => { let i = f.code[f.pc + 1] as usize; let v = f.pop(); f.set_local(i, v); f.pc += 2 }
            0x37 | 0x39 => { let i = f.code[f.pc + 1] as usize; f.pop(); let v = f.pop(); f.set_local(i, v); f.pc += 2 }
            0x3b..=0x3e => { let v = f.pop(); f.set_local((op - 0x3b) as usize, v); f.pc += 1 }
            0x3f..=0x42 => { f.pop(); let v = f.pop(); f.set_local((op - 0x3f) as usize, v); f.pc += 1 }
            0x43..=0x46 => { let v = f.pop(); f.set_local((op - 0x43) as usize, v); f.pc += 1 }
            0x47..=0x4a => { f.pop(); let v = f.pop(); f.set_local((op - 0x47) as usize, v); f.pc += 1 }
            0x4b..=0x4e => { let v = f.pop(); f.set_local((op - 0x4b) as usize, v); f.pc += 1 }
            // array stores
            0x4f => { let v = f.pop_i(); let (a, i) = s::array_index(t, f)?; unsafe { *obj::array_data::<i32>(a).add(i) = v }; f.pc += 1 }
            0x50 => { let v = f.pop_l(); let (a, i) = s::array_index(t, f)?; unsafe { *obj::array_data::<i64>(a).add(i) = v }; f.pc += 1 }
            0x51 => { let v = f.pop() as u32; let (a, i) = s::array_index(t, f)?; unsafe { *obj::array_data::<u32>(a).add(i) = v }; f.pc += 1 }
            0x52 => { let v = f.pop_l() as u64; let (a, i) = s::array_index(t, f)?; unsafe { *obj::array_data::<u64>(a).add(i) = v }; f.pc += 1 }
            0x53 => { s::aastore(t, f)?; f.pc += 1 }
            0x54 => { let v = f.pop_i(); let (a, i) = s::array_index(t, f)?; s::bastore(a, i, v); f.pc += 1 }
            0x55 | 0x56 => { let v = f.pop_i() as u16; let (a, i) = s::array_index(t, f)?; unsafe { *obj::array_data::<u16>(a).add(i) = v }; f.pc += 1 }
            // stack
            0x57 => { f.pop(); f.pc += 1 }
            0x58 => { f.pop(); f.pop(); f.pc += 1 }
            0x59 => { let v = f.peek(0); f.push(v); f.pc += 1 }
            0x5a => { let a = f.pop(); let b = f.pop(); f.push(a); f.push(b); f.push(a); f.pc += 1 }
            0x5b => { let a = f.pop(); let b = f.pop(); let c = f.pop(); f.push(a); f.push(c); f.push(b); f.push(a); f.pc += 1 }
            0x5c => { let a = f.peek(1); let b = f.peek(0); f.push(a); f.push(b); f.pc += 1 }
            0x5d => { let a = f.pop(); let b = f.pop(); let c = f.pop(); f.push(b); f.push(a); f.push(c); f.push(b); f.push(a); f.pc += 1 }
            0x5e => { let a = f.pop(); let b = f.pop(); let c = f.pop(); let d = f.pop(); f.push(b); f.push(a); f.push(d); f.push(c); f.push(b); f.push(a); f.pc += 1 }
            0x5f => { let a = f.pop(); let b = f.pop(); f.push(a); f.push(b); f.pc += 1 }
            // arithmetic
            0x60 => { i32_op!(f, i32::wrapping_add); f.pc += 1 }
            0x61 => { i64_op!(f, i64::wrapping_add); f.pc += 1 }
            0x62 => { f32_op!(f, |a: f32, b: f32| a + b); f.pc += 1 }
            0x63 => { f64_op!(f, |a: f64, b: f64| a + b); f.pc += 1 }
            0x64 => { i32_op!(f, i32::wrapping_sub); f.pc += 1 }
            0x65 => { i64_op!(f, i64::wrapping_sub); f.pc += 1 }
            0x66 => { f32_op!(f, |a: f32, b: f32| a - b); f.pc += 1 }
            0x67 => { f64_op!(f, |a: f64, b: f64| a - b); f.pc += 1 }
            0x68 => { i32_op!(f, i32::wrapping_mul); f.pc += 1 }
            0x69 => { i64_op!(f, i64::wrapping_mul); f.pc += 1 }
            0x6a => { f32_op!(f, |a: f32, b: f32| a * b); f.pc += 1 }
            0x6b => { f64_op!(f, |a: f64, b: f64| a * b); f.pc += 1 }
            0x6c | 0x70 => {
                let b = f.pop_i(); let a = f.pop_i();
                if b == 0 { return Err(throw_new(t, "java/lang/ArithmeticException", "/ by zero")) }
                f.push_i(if op == 0x6c { a.wrapping_div(b) } else { a.wrapping_rem(b) }); f.pc += 1
            }
            0x6d | 0x71 => {
                let b = f.pop_l(); let a = f.pop_l();
                if b == 0 { return Err(throw_new(t, "java/lang/ArithmeticException", "/ by zero")) }
                f.push_l(if op == 0x6d { a.wrapping_div(b) } else { a.wrapping_rem(b) }); f.pc += 1
            }
            0x6e => { f32_op!(f, |a: f32, b: f32| a / b); f.pc += 1 }
            0x6f => { f64_op!(f, |a: f64, b: f64| a / b); f.pc += 1 }
            0x72 => { f32_op!(f, |a: f32, b: f32| a % b); f.pc += 1 }
            0x73 => { f64_op!(f, |a: f64, b: f64| a % b); f.pc += 1 }
            0x74 => { let a = f.pop_i(); f.push_i(a.wrapping_neg()); f.pc += 1 }
            0x75 => { let a = f.pop_l(); f.push_l(a.wrapping_neg()); f.pc += 1 }
            0x76 => { let a = f.pop_f(); f.push_f(-a); f.pc += 1 }
            0x77 => { let a = f.pop_d(); f.push_d(-a); f.pc += 1 }
            0x78 => { i32_op!(f, |a: i32, b: i32| a.wrapping_shl(b as u32 & 31)); f.pc += 1 }
            0x79 => { let b = f.pop_i(); let a = f.pop_l(); f.push_l(a.wrapping_shl(b as u32 & 63)); f.pc += 1 }
            0x7a => { i32_op!(f, |a: i32, b: i32| a.wrapping_shr(b as u32 & 31)); f.pc += 1 }
            0x7b => { let b = f.pop_i(); let a = f.pop_l(); f.push_l(a.wrapping_shr(b as u32 & 63)); f.pc += 1 }
            0x7c => { i32_op!(f, |a: i32, b: i32| ((a as u32) >> (b as u32 & 31)) as i32); f.pc += 1 }
            0x7d => { let b = f.pop_i(); let a = f.pop_l(); f.push_l(((a as u64) >> (b as u32 & 63)) as i64); f.pc += 1 }
            0x7e => { i32_op!(f, |a: i32, b: i32| a & b); f.pc += 1 }
            0x7f => { i64_op!(f, |a: i64, b: i64| a & b); f.pc += 1 }
            0x80 => { i32_op!(f, |a: i32, b: i32| a | b); f.pc += 1 }
            0x81 => { i64_op!(f, |a: i64, b: i64| a | b); f.pc += 1 }
            0x82 => { i32_op!(f, |a: i32, b: i32| a ^ b); f.pc += 1 }
            0x83 => { i64_op!(f, |a: i64, b: i64| a ^ b); f.pc += 1 }
            0x84 => {
                let i = f.code[f.pc + 1] as usize;
                let v = (f.local(i) as i32).wrapping_add(f.code[f.pc + 2] as i8 as i32);
                f.set_local(i, v as i64 as u64); f.pc += 3
            }
            // conversions
            0x85 => { let a = f.pop_i(); f.push_l(a as i64); f.pc += 1 }
            0x86 => { let a = f.pop_i(); f.push_f(a as f32); f.pc += 1 }
            0x87 => { let a = f.pop_i(); f.push_d(a as f64); f.pc += 1 }
            0x88 => { let a = f.pop_l(); f.push_i(a as i32); f.pc += 1 }
            0x89 => { let a = f.pop_l(); f.push_f(a as f32); f.pc += 1 }
            0x8a => { let a = f.pop_l(); f.push_d(a as f64); f.pc += 1 }
            0x8b => { let a = f.pop_f(); f.push_i(a as i32); f.pc += 1 }
            0x8c => { let a = f.pop_f(); f.push_l(a as i64); f.pc += 1 }
            0x8d => { let a = f.pop_f(); f.push_d(a as f64); f.pc += 1 }
            0x8e => { let a = f.pop_d(); f.push_i(a as i32); f.pc += 1 }
            0x8f => { let a = f.pop_d(); f.push_l(a as i64); f.pc += 1 }
            0x90 => { let a = f.pop_d(); f.push_f(a as f32); f.pc += 1 }
            0x91 => { let a = f.pop_i(); f.push_i(a as i8 as i32); f.pc += 1 }
            0x92 => { let a = f.pop_i(); f.push_i(a as u16 as i32); f.pc += 1 }
            0x93 => { let a = f.pop_i(); f.push_i(a as i16 as i32); f.pc += 1 }
            // comparisons
            0x94 => { let b = f.pop_l(); let a = f.pop_l(); f.push_i(a.cmp(&b) as i32); f.pc += 1 }
            0x95 | 0x96 => { let b = f.pop_f(); let a = f.pop_f(); f.push_i(s::fcmp(a as f64, b as f64, op == 0x96)); f.pc += 1 }
            0x97 | 0x98 => { let b = f.pop_d(); let a = f.pop_d(); f.push_i(s::fcmp(a, b, op == 0x98)); f.pc += 1 }
            0x99..=0x9e => { let a = f.pop_i(); f.branch_if(s::int_cond(op - 0x99, a, 0)) }
            0x9f..=0xa4 => { let b = f.pop_i(); let a = f.pop_i(); f.branch_if(s::int_cond(op - 0x9f, a, b)) }
            0xa5 | 0xa6 => { let b = f.pop(); let a = f.pop(); f.branch_if((a == b) == (op == 0xa5)) }
            0xa7 => f.branch_if(true),
            0xa8 | 0xa9 => return Err(throw_new(t, "java/lang/VerifyError", "jsr/ret are not supported")),
            0xaa => s::tableswitch(f),
            0xab => s::lookupswitch(f),
            0xac | 0xae | 0xb0 => return Ok(Some(f.pop())),
            0xad | 0xaf => { f.pop(); return Ok(Some(f.pop())) }
            0xb1 => return Ok(Some(0)),
            0xb2 => { s::getstatic(t, f)?; f.pc += 3 }
            0xb3 => { s::putstatic(t, f)?; f.pc += 3 }
            0xb4 => { s::getfield(t, f)?; f.pc += 3 }
            0xb5 => { s::putfield(t, f)?; f.pc += 3 }
            0xb6..=0xb9 => { s::invoke(t, f, op)?; f.pc += if op == 0xb9 { 5 } else { 3 } }
            0xba => { s::invokedynamic(t, f)?; f.pc += 5 }
            0xbb => { s::new_object(t, f)?; f.pc += 3 }
            0xbc => { s::newarray(t, f)?; f.pc += 2 }
            0xbd => { s::anewarray(t, f)?; f.pc += 3 }
            0xbe => {
                let a: Oop = f.pop_ref();
                if a.is_null() { return Err(crate::runtime::null_pointer(t)) }
                f.push_i(unsafe { obj::array_length(a) }); f.pc += 1
            }
            0xbf => { let ex = f.pop_ref(); return Err(s::athrow(t, ex)) }
            0xc0 => { s::checkcast(t, f)?; f.pc += 3 }
            0xc1 => { s::instanceof(t, f)?; f.pc += 3 }
            0xc2 => {
                let o = f.pop_ref();
                if o.is_null() { return Err(crate::runtime::null_pointer(t)) }
                crate::sync::enter(o); f.pc += 1
            }
            0xc3 => {
                let o = f.pop_ref();
                if o.is_null() { return Err(crate::runtime::null_pointer(t)) }
                if !crate::sync::exit(o) { return Err(throw_new(t, "java/lang/IllegalMonitorStateException", "")) }
                f.pc += 1
            }
            0xc4 => s::wide(f),
            0xc5 => { s::multianewarray(t, f)?; f.pc += 4 }
            0xc6 | 0xc7 => { let a = f.pop(); f.branch_if((a == 0) == (op == 0xc6)) }
            0xc8 => { let off = f.i4(f.pc + 1); f.pc = (f.pc as i64 + off as i64) as usize }
            _ => return Err(throw_new(t, "java/lang/VerifyError", &format!("bad opcode {op:#x}"))),
        }
    }
}
