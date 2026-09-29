//! Operand-stack frame and the non-trivial bytecode semantics.

use crate::classfile::{CodeInfo, Cp};
use crate::heap::{obj, Oop};
use crate::klass::{Klass, KlassKind, ACC_ABSTRACT, ACC_FINAL, ACC_INTERFACE};
use crate::link::{select_method, Resolved};
use crate::method::Method;
use crate::runtime::{self as rt, link_error, null_pointer, throw_new};
use crate::thread::{JResult, Thread, Throw};
use crate::types::BasicType;
use crate::vm::vm;
use std::sync::atomic::Ordering;

pub(super) struct Frame {
    pub method: &'static Method,
    pub code: &'static [u8],
    pub info: &'static CodeInfo,
    pub locals: *mut u64,
    pub stack_base: *mut u64,
    pub sp: *mut u64,
    pub pc: usize,
    pub op_pc: usize,
}

// All slot accesses stay inside the frame the verifier-shaped bytecode describes: locals below
// `max_locals`, operand stack below `max_stack` (the value stack was bounds-checked on entry).
impl Frame {
    pub fn new(method: &'static Method, info: &'static CodeInfo, locals: *mut u64) -> Frame {
        // SAFETY: `invoke` reserved max_locals + max_stack slots from `locals`.
        let stack_base = unsafe { locals.add(info.max_locals as usize) };
        Frame { method, code: info.code, info, locals, stack_base, sp: stack_base, pc: 0, op_pc: 0 }
    }
    #[inline(always)]
    pub fn push(&mut self, v: u64) {
        unsafe { *self.sp = v; self.sp = self.sp.add(1) }
    }
    #[inline(always)]
    pub fn pop(&mut self) -> u64 {
        unsafe { self.sp = self.sp.sub(1); *self.sp }
    }
    #[inline(always)]
    pub fn peek(&self, depth: usize) -> u64 {
        unsafe { *self.sp.sub(depth + 1) }
    }
    #[inline(always)]
    pub fn local(&self, i: usize) -> u64 {
        unsafe { *self.locals.add(i) }
    }
    #[inline(always)]
    pub fn set_local(&mut self, i: usize, v: u64) {
        unsafe { *self.locals.add(i) = v }
    }
    #[inline(always)] pub fn push_i(&mut self, v: i32) { self.push(v as i64 as u64) }
    #[inline(always)] pub fn pop_i(&mut self) -> i32 { self.pop() as i32 }
    #[inline(always)] pub fn push_l(&mut self, v: i64) { self.push(v as u64); self.push(0) }
    #[inline(always)] pub fn pop_l(&mut self) -> i64 { self.pop(); self.pop() as i64 }
    #[inline(always)] pub fn push_f(&mut self, v: f32) { self.push(v.to_bits() as u64) }
    #[inline(always)] pub fn pop_f(&mut self) -> f32 { f32::from_bits(self.pop() as u32) }
    #[inline(always)] pub fn push_d(&mut self, v: f64) { self.push(v.to_bits()); self.push(0) }
    #[inline(always)] pub fn pop_d(&mut self) -> f64 { self.pop(); f64::from_bits(self.pop()) }
    #[inline(always)] pub fn push_ref(&mut self, v: Oop) { self.push(v as u64) }
    #[inline(always)] pub fn pop_ref(&mut self) -> Oop { self.pop() as Oop }
    #[inline(always)]
    pub fn u2(&self, at: usize) -> u16 {
        u16::from_be_bytes([self.code[at], self.code[at + 1]])
    }
    #[inline(always)]
    pub fn i4(&self, at: usize) -> i32 {
        i32::from_be_bytes([self.code[at], self.code[at + 1], self.code[at + 2], self.code[at + 3]])
    }
    #[inline(always)]
    pub fn branch_if(&mut self, taken: bool) {
        self.pc = if taken { (self.op_pc as i64 + self.u2(self.op_pc + 1) as i16 as i64) as usize } else { self.op_pc + 3 };
    }
    fn holder(&self) -> &'static Klass {
        self.method.holder
    }
    /// Pushes a value of type `ty` given in slot encoding.
    fn push_typed(&mut self, ty: BasicType, v: u64) {
        self.push(v);
        if ty.slots() == 2 {
            self.push(0);
        }
    }
    fn pop_typed(&mut self, ty: BasicType) -> u64 {
        if ty.slots() == 2 {
            self.pop();
        }
        self.pop()
    }
}

pub(super) fn fcmp(a: f64, b: f64, nan_is_greater: bool) -> i32 {
    match a.partial_cmp(&b) {
        Some(o) => o as i32,
        None => if nan_is_greater { 1 } else { -1 },
    }
}

pub(super) fn int_cond(cond: u8, a: i32, b: i32) -> bool {
    match cond {
        0 => a == b,
        1 => a != b,
        2 => a < b,
        3 => a >= b,
        4 => a > b,
        _ => a <= b,
    }
}

pub(super) fn tableswitch(f: &mut Frame) {
    let p = (f.op_pc + 4) & !3;
    let (default, low, high) = (f.i4(p), f.i4(p + 4), f.i4(p + 8));
    let idx = f.pop_i();
    let off = if idx < low || idx > high { default } else { f.i4(p + 12 + 4 * (idx - low) as usize) };
    f.pc = (f.op_pc as i64 + off as i64) as usize;
}

pub(super) fn lookupswitch(f: &mut Frame) {
    let p = (f.op_pc + 4) & !3;
    let (default, n) = (f.i4(p), f.i4(p + 4) as usize);
    let key = f.pop_i();
    let (mut lo, mut hi) = (0usize, n);
    let mut off = default;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let k = f.i4(p + 8 + mid * 8);
        if k == key {
            off = f.i4(p + 12 + mid * 8);
            break;
        } else if k < key {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    f.pc = (f.op_pc as i64 + off as i64) as usize;
}

pub(super) fn wide(f: &mut Frame) {
    let op = f.code[f.pc + 1];
    let i = f.u2(f.pc + 2) as usize;
    match op {
        0x15 | 0x17 | 0x19 => f.push(f.local(i)),
        0x16 | 0x18 => { f.push(f.local(i)); f.push(0) }
        0x36 | 0x38 | 0x3a => { let v = f.pop(); f.set_local(i, v) }
        0x37 | 0x39 => { f.pop(); let v = f.pop(); f.set_local(i, v) }
        0x84 => {
            let delta = f.u2(f.pc + 4) as i16 as i32;
            f.set_local(i, (f.local(i) as i32).wrapping_add(delta) as i64 as u64);
            f.pc += 6;
            return;
        }
        _ => {}
    }
    f.pc += 4;
}

/// Pops `index, array`; null- and bounds-checks; returns the array and the index.
pub(super) fn array_index(t: &mut Thread, f: &mut Frame) -> JResult<(Oop, usize)> {
    let i = f.pop_i();
    let a = f.pop_ref();
    if a.is_null() {
        return Err(null_pointer(t));
    }
    // SAFETY: non-null array reference.
    let len = unsafe { obj::array_length(a) };
    if i < 0 || i >= len {
        let msg = format!("Index {i} out of bounds for length {len}");
        return Err(throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", &msg));
    }
    Ok((a, i as usize))
}

pub(super) fn bastore(a: Oop, i: usize, v: i32) {
    // SAFETY: bounds were checked by `array_index`.
    unsafe {
        let is_bool = matches!(obj::klass(a).kind, KlassKind::TypeArray { elem: BasicType::Boolean });
        *obj::array_data::<i8>(a).add(i) = if is_bool { (v & 1) as i8 } else { v as i8 };
    }
}

pub(super) fn aastore(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let v = f.pop_ref();
    let (a, i) = array_index(t, f)?;
    if !v.is_null() {
        // SAFETY: live array and value.
        let (elem, vk) = unsafe {
            let KlassKind::ObjArray { elem } = obj::klass(a).kind else { unreachable!() };
            (elem, obj::klass(v))
        };
        if !vk.is_subtype_of(elem) {
            return Err(throw_new(t, "java/lang/ArrayStoreException", &rt::class_name_of(v)));
        }
    }
    // SAFETY: bounds checked.
    unsafe { *obj::array_data::<Oop>(a).add(i) = v };
    Ok(())
}

/// Truncates a slot value to the representation a field of type `ty` holds.
fn normalize(ty: BasicType, v: u64) -> u64 {
    match ty {
        BasicType::Boolean => v & 1,
        BasicType::Byte => v as i8 as i64 as u64,
        BasicType::Char => v as u16 as u64,
        BasicType::Short => v as i16 as i64 as u64,
        BasicType::Int => v as i32 as i64 as u64,
        BasicType::Float => v as u32 as u64,
        _ => v,
    }
}

pub(super) fn getstatic(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let r = f.holder().resolve_field(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    let Resolved::StaticField { holder, slot, ty } = r else {
        return Err(throw_new(t, "java/lang/IncompatibleClassChangeError", "expected a static field"));
    };
    rt::initialize(t, holder)?;
    // Unsafe may have written a narrower value into the slot; re-normalize on read.
    f.push_typed(ty, normalize(ty, slot.load(Ordering::Relaxed)));
    Ok(())
}

pub(super) fn putstatic(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let r = f.holder().resolve_field(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    let Resolved::StaticField { holder, slot, ty } = r else {
        return Err(throw_new(t, "java/lang/IncompatibleClassChangeError", "expected a static field"));
    };
    rt::initialize(t, holder)?;
    let v = f.pop_typed(ty);
    slot.store(normalize(ty, v), Ordering::Relaxed);
    Ok(())
}

pub(super) fn getfield(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let r = f.holder().resolve_field(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    let Resolved::InstanceField { offset, ty } = r else {
        return Err(throw_new(t, "java/lang/IncompatibleClassChangeError", "expected an instance field"));
    };
    let o = f.pop_ref();
    if o.is_null() {
        return Err(null_pointer(t));
    }
    // SAFETY: live object; offset from its class layout.
    let v = unsafe { read_field(o, offset, ty) };
    f.push_typed(ty, v);
    Ok(())
}

pub(super) fn putfield(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let r = f.holder().resolve_field(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    let Resolved::InstanceField { offset, ty } = r else {
        return Err(throw_new(t, "java/lang/IncompatibleClassChangeError", "expected an instance field"));
    };
    let v = f.pop_typed(ty);
    let o = f.pop_ref();
    if o.is_null() {
        return Err(null_pointer(t));
    }
    // SAFETY: live object; offset from its class layout.
    unsafe { write_field(o, offset, ty, v) };
    Ok(())
}

/// Reads a field in slot encoding.
pub(crate) unsafe fn read_field(o: Oop, offset: u32, ty: BasicType) -> u64 {
    match ty {
        BasicType::Boolean | BasicType::Byte => obj::read::<i8>(o, offset) as i64 as u64,
        BasicType::Char => obj::read::<u16>(o, offset) as u64,
        BasicType::Short => obj::read::<i16>(o, offset) as i64 as u64,
        BasicType::Int => obj::read::<i32>(o, offset) as i64 as u64,
        BasicType::Float => obj::read::<u32>(o, offset) as u64,
        _ => obj::read::<u64>(o, offset),
    }
}

/// Writes a slot-encoded value into a field.
pub(crate) unsafe fn write_field(o: Oop, offset: u32, ty: BasicType, v: u64) {
    match ty {
        BasicType::Boolean => obj::write::<u8>(o, offset, (v & 1) as u8),
        BasicType::Byte => obj::write::<u8>(o, offset, v as u8),
        BasicType::Char | BasicType::Short => obj::write::<u16>(o, offset, v as u16),
        BasicType::Int | BasicType::Float => obj::write::<u32>(o, offset, v as u32),
        _ => obj::write::<u64>(o, offset, v),
    }
}

pub(super) fn ldc(t: &mut Thread, f: &mut Frame, index: u16) -> JResult<()> {
    let k = f.holder();
    let cf = k.cf.as_ref().unwrap();
    match cf.pool[index as usize] {
        Cp::Int(v) => f.push_i(v),
        Cp::Float(v) => f.push_f(v),
        Cp::Long(v) => f.push_l(v),
        Cp::Double(v) => f.push_d(v),
        Cp::String(s) => {
            if let Some(Resolved::String(o)) = k.resolved[index as usize].get() {
                f.push(*o as u64);
                return Ok(());
            }
            let o = rt::intern_literal(t, cf.utf8_bytes(s).unwrap_or(&[]))?;
            let _ = k.resolved[index as usize].set(Resolved::String(o as usize));
            f.push_ref(o);
        }
        Cp::Class(_) => {
            let c = k.resolve_class(index).map_err(|e| link_error(t, e))?;
            let m = rt::mirror_of(t, c)?;
            f.push_ref(m);
        }
        _ => return Err(throw_new(t, "java/lang/UnsupportedOperationException", "ldc of a dynamic constant")),
    }
    Ok(())
}

/// invokevirtual / invokespecial / invokestatic / invokeinterface.
pub(super) fn invoke(t: &mut Thread, f: &mut Frame, op: u8) -> JResult<()> {
    let m = f.holder().resolve_method(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    if m.access & 0x0080 != 0 && m.is_native() && !m.is_static() {
        return Err(throw_new(t, "java/lang/UnsupportedOperationException", "signature-polymorphic call"));
    }
    let n = m.param_slots() as usize;
    // SAFETY: the operand stack holds the n argument slots.
    let args = unsafe { f.sp.sub(n) };
    let target = if op == 0xb8 {
        if !m.is_static() {
            return Err(throw_new(t, "java/lang/IncompatibleClassChangeError", &m.to_string()));
        }
        rt::initialize(t, m.holder)?;
        m
    } else {
        // SAFETY: slot 0 of the arguments is the receiver.
        let recv = unsafe { *args } as Oop;
        if recv.is_null() {
            return Err(null_pointer(t));
        }
        if op == 0xb7 {
            special_target(f.holder(), m)
        } else if m.is_private() || m.access & ACC_FINAL != 0 {
            m
        } else {
            // SAFETY: live receiver.
            let rk = unsafe { obj::klass(recv) };
            select_method(rk, m).ok_or_else(|| throw_new(t, "java/lang/AbstractMethodError", &m.to_string()))?
        }
    };
    let ret = super::invoke(t, target, args)?;
    f.sp = args;
    if m.sig.ret != BasicType::Void {
        f.push_typed(m.sig.ret, ret);
    }
    Ok(())
}

/// invokespecial selection (JVMS 6.5): super calls search from the current class's superclass.
fn special_target(current: &'static Klass, m: &'static Method) -> &'static Method {
    const ACC_SUPER: u16 = 0x0020;
    let is_super_call = m.name.as_str() != "<init>"
        && !m.holder.is_interface()
        && !std::ptr::eq(current, m.holder)
        && current.access & ACC_SUPER != 0
        && current.is_subtype_of(m.holder);
    if !is_super_call {
        return m;
    }
    current
        .super_klass
        .and_then(|s| s.supers().find_map(|k| k.declared_method(m.name, m.desc)))
        .filter(|c| !c.is_abstract())
        .unwrap_or(m)
}

pub(super) fn invokedynamic(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let _ = f;
    Err(throw_new(t, "java/lang/UnsupportedOperationException", "invokedynamic is not linked yet"))
}

pub(super) fn new_object(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let k = f.holder().resolve_class(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    if k.access & (ACC_ABSTRACT | ACC_INTERFACE) != 0 {
        return Err(throw_new(t, "java/lang/InstantiationError", &k.name.as_str().replace('/', ".")));
    }
    rt::initialize(t, k)?;
    let o = rt::alloc_instance(t, k)?;
    f.push_ref(o);
    Ok(())
}

pub(super) fn newarray(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let ty = BasicType::from_newarray_code(f.code[f.pc + 1])
        .ok_or_else(|| throw_new(t, "java/lang/VerifyError", "bad newarray type"))?;
    let k = vm().primitive_array_class(ty).map_err(|e| link_error(t, e.into()))?;
    let len = f.pop_i();
    let a = rt::alloc_array(t, k, len)?;
    f.push_ref(a);
    Ok(())
}

pub(super) fn anewarray(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let elem = f.holder().resolve_class(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    let k = vm().array_class_of(elem).map_err(|e| link_error(t, e.into()))?;
    let len = f.pop_i();
    let a = rt::alloc_array(t, k, len)?;
    f.push_ref(a);
    Ok(())
}

pub(super) fn multianewarray(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let k = f.holder().resolve_class(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    let dims = f.code[f.pc + 3] as usize;
    let mut counts = vec![0i32; dims];
    for c in counts.iter_mut().rev() {
        *c = f.pop_i();
    }
    if let Some(&bad) = counts.iter().find(|&&c| c < 0) {
        return Err(throw_new(t, "java/lang/NegativeArraySizeException", &bad.to_string()));
    }
    let a = build_multi(t, k, &counts)?;
    f.push_ref(a);
    Ok(())
}

fn build_multi(t: &mut Thread, k: &'static Klass, counts: &[i32]) -> JResult<Oop> {
    let a = rt::alloc_array(t, k, counts[0])?;
    if counts.len() > 1 {
        let KlassKind::ObjArray { elem } = k.kind else { return Ok(a) };
        for i in 0..counts[0] as usize {
            let sub = build_multi(t, elem, &counts[1..])?;
            // SAFETY: `a` has counts[0] reference elements.
            unsafe { *obj::array_data::<Oop>(a).add(i) = sub };
        }
    }
    Ok(a)
}

pub(super) fn athrow(t: &mut Thread, ex: Oop) -> Throw {
    if ex.is_null() { null_pointer(t) } else { Throw(ex) }
}

pub(super) fn checkcast(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let o = f.peek(0) as Oop;
    if o.is_null() {
        return Ok(());
    }
    let k = f.holder().resolve_class(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    // SAFETY: live object.
    if unsafe { obj::klass(o) }.is_subtype_of(k) {
        return Ok(());
    }
    let msg = format!("class {} cannot be cast to class {}", rt::class_name_of(o), k.name.as_str().replace('/', "."));
    Err(throw_new(t, "java/lang/ClassCastException", &msg))
}

pub(super) fn instanceof(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let o = f.pop_ref();
    if o.is_null() {
        f.push_i(0);
        return Ok(());
    }
    let k = f.holder().resolve_class(f.u2(f.pc + 1)).map_err(|e| link_error(t, e))?;
    // SAFETY: live object.
    f.push_i(unsafe { obj::klass(o) }.is_subtype_of(k) as i32);
    Ok(())
}

/// The handler pc for an exception thrown at `f.op_pc`, if this method catches it.
pub(super) fn find_handler(t: &mut Thread, f: &Frame, ex: Oop) -> Option<usize> {
    let _ = t;
    // SAFETY: live exception object.
    let ek = unsafe { obj::klass(ex) };
    let pc = f.op_pc as u16;
    for h in f.info.handlers.iter().filter(|h| h.start <= pc && pc < h.end) {
        if h.catch_type == 0 {
            return Some(h.handler as usize);
        }
        match f.holder().resolve_class(h.catch_type) {
            Ok(c) if ek.is_subtype_of(c) => return Some(h.handler as usize),
            _ => {}
        }
    }
    None
}
