//! Inlining decisions, made once per compilation before any code is emitted (resolution may
//! progress concurrently, and the frame size must be known for the prologue).
//!
//! A call site is inlined when its target is statically bound (static, private, final,
//! `invokespecial`, or a final class) and small (HotSpot's C1 limits: 35 bytecodes, or any size
//! for `@ForceInline`; never `@DontInline`, recursive, synchronized or with handlers). Two
//! strategies (`inline.rs`):
//! - **Leaf**: a side-effect-free body with no calls or loops runs in a scratch area; any slow
//!   path restarts the call as an ordinary one.
//! - **Framed**: any other small body runs in a real interpreter frame placed at the arguments,
//!   as a call would place it, without a native call.

use super::analysis::{analyze, i2, i4, insn_len, Analysis};
use super::intrinsics::intrinsic;
use crate::classfile::Cp;
use crate::klass::ACC_FINAL;
use crate::link::Resolved;
use crate::method::Method;
use crate::thread::header;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;

/// HotSpot's `MaxInlineSize` / `C1MaxInlineSize`.
const MAX_INLINE_SIZE: usize = 35;
/// Leaves up to this size are inlined once the callee is hot (cf. `FreqInlineSize`).
const HOT_LEAF_SIZE: usize = 128;
const HOT_CALLEE: u32 = 1000;
/// Scratch slots one leaf body may use.
const MAX_LEAF_SLOTS: usize = 48;
/// Nesting of inlined frames (cf. `C1MaxInlineLevel`).
const MAX_DEPTH: usize = 6;
/// `@ForceInline` bodies above this size are still left as calls.
const MAX_FORCED_SIZE: usize = 400;

pub(super) enum Kind {
    Leaf,
    Framed(Rc<Plan>),
}

pub(super) struct Site {
    pub target: &'static Method,
    pub kind: Kind,
}

/// The inlined call sites of one method body, keyed by bci, and the highest frame slot (from
/// the frame base) the body and everything inlined into it may touch.
#[derive(Default)]
pub(super) struct Plan {
    pub sites: HashMap<usize, Site>,
    pub extent: i32,
}

/// Plans `m`'s body whose operand stack starts at frame slot `stack_base`.
pub(super) fn plan(m: &'static Method, an: &Analysis, stack_base: i32, chain: &mut Vec<&'static Method>) -> Plan {
    let info = m.code.expect("planned method has code");
    let scratch = stack_base + info.max_stack as i32 + 2;
    let mut out = Plan { sites: HashMap::new(), extent: scratch };
    if std::env::var_os("RAVA_CORE_NO_INLINE").is_some() {
        return out;
    }
    chain.push(m);
    let code = info.code;
    let mut pc = 0;
    while pc < code.len() {
        if let (Some(depth), 0xb6..=0xb8) = (an.depth[pc], code[pc]) {
            if let Some(t) = bound_target(m, code, pc) {
                if is_leaf(t) {
                    out.extent = out.extent.max(scratch + leaf_slots(t));
                    out.sites.insert(pc, Site { target: t, kind: Kind::Leaf });
                } else if let Some(callee_an) = framed_ok(t, chain) {
                    let base = stack_base + depth as i32 - t.param_slots() as i32;
                    let callee_stack = base + t.code.unwrap().max_locals as i32 + header::SLOTS as i32;
                    let nested = plan(t, &callee_an, callee_stack, chain);
                    out.extent = out.extent.max(nested.extent);
                    out.sites.insert(pc, Site { target: t, kind: Kind::Framed(Rc::new(nested)) });
                }
            }
        }
        pc += insn_len(code, pc);
    }
    chain.pop();
    out
}

/// The statically bound target of the call at `pc`, excluding method-handle invokers and
/// intrinsics (compiled separately).
fn bound_target(caller: &'static Method, code: &[u8], pc: usize) -> Option<&'static Method> {
    let index = u16::from_be_bytes([code[pc + 1], code[pc + 2]]);
    let Some(Resolved::Method(m)) = caller.holder.resolved[index as usize].get().copied() else { return None };
    if m.is_signature_polymorphic() || m.is_abstract() || intrinsic(m).is_some() {
        return None;
    }
    let target = match code[pc] {
        0xb8 if m.is_static() => m,
        0xb7 if !m.is_static() => crate::interp::special_target(caller.holder, m),
        0xb6 if !m.is_static() && (m.is_private() || m.access & ACC_FINAL != 0 || m.holder.access & ACC_FINAL != 0) => m,
        _ => return None,
    };
    (!target.is_abstract() && !target.is_native()).then_some(target)
}

fn framed_ok(t: &'static Method, chain: &[&'static Method]) -> Option<Analysis> {
    let info = t.code?;
    if chain.len() > MAX_DEPTH || chain.iter().any(|c| std::ptr::eq(*c, t)) || t.is_synchronized() || !info.handlers.is_empty() {
        return None;
    }
    if t.has_annotation("Ljdk/internal/vm/annotation/DontInline;") {
        return None;
    }
    let forced = t.has_annotation("Ljdk/internal/vm/annotation/ForceInline;");
    let limit = if forced { MAX_FORCED_SIZE } else { MAX_INLINE_SIZE };
    if info.code.len() > limit {
        return None;
    }
    let an = analyze(t)?;
    let mut pc = 0;
    while pc < info.code.len() {
        if matches!(info.code[pc], 0xa8 | 0xa9 | 0xc9) {
            return None;
        }
        pc += insn_len(info.code, pc);
    }
    Some(an)
}

/// Scratch slots a leaf body needs.
pub(super) fn leaf_slots(m: &Method) -> i32 {
    m.code.map_or(0, |c| c.max_locals as i32 + c.max_stack as i32)
}

fn is_leaf(m: &'static Method) -> bool {
    let Some(info) = m.code else { return false };
    let limit = if m.invocations.load(Ordering::Relaxed) >= HOT_CALLEE { HOT_LEAF_SIZE } else { MAX_INLINE_SIZE };
    if m.is_synchronized() || !info.handlers.is_empty() || info.code.len() > limit {
        return false;
    }
    if info.max_locals as usize + info.max_stack as usize > MAX_LEAF_SLOTS || m.has_annotation("Ljdk/internal/vm/annotation/DontInline;") {
        return false;
    }
    let (code, resolved) = (info.code, &m.holder.resolved);
    let is = |pc: usize, f: fn(Resolved) -> bool| resolved[u16::from_be_bytes([code[pc + 1], code[pc + 2]]) as usize].get().copied().is_some_and(f);
    let mut pc = 0;
    while pc < code.len() {
        let ok = match code[pc] {
            0x00..=0x11 | 0x15..=0x35 | 0x36..=0x4e | 0x57..=0x98 | 0xac..=0xb1 | 0xbe => true,
            0x12..=0x14 => ldc_is_constant(m, code, pc),
            0x99..=0xa7 | 0xc6 | 0xc7 => i2(code, pc + 1) > 0,
            0xaa | 0xab => switch_targets(code, pc).iter().all(|&t| t > pc),
            0xb2 => is(pc, |r| matches!(r, Resolved::StaticField { .. })),
            0xb4 => is(pc, |r| matches!(r, Resolved::InstanceField { .. })),
            0xb6 | 0xb8 => is(pc, |r| matches!(r, Resolved::Method(t) if intrinsic(t).is_some_and(|i| i.is_pure()))),
            _ => false,
        };
        if !ok {
            return false;
        }
        pc += insn_len(code, pc);
    }
    true
}

fn ldc_is_constant(m: &Method, code: &[u8], pc: usize) -> bool {
    let index = if code[pc] == 0x12 { code[pc + 1] as u16 } else { u16::from_be_bytes([code[pc + 1], code[pc + 2]]) };
    let Some(cf) = m.holder.cf.as_ref() else { return false };
    match cf.pool[index as usize] {
        Cp::Int(_) | Cp::Float(_) | Cp::Long(_) | Cp::Double(_) => true,
        Cp::String(_) => matches!(m.holder.resolved[index as usize].get(), Some(Resolved::String(_))),
        _ => false,
    }
}

fn switch_targets(code: &[u8], pc: usize) -> Vec<usize> {
    let p = (pc + 4) & !3;
    let rel = |off: i32| (pc as i64 + off as i64) as usize;
    let mut out = vec![rel(i4(code, p))];
    if code[pc] == 0xaa {
        let n = (i4(code, p + 8) - i4(code, p + 4) + 1) as usize;
        out.extend((0..n).map(|i| rel(i4(code, p + 12 + 4 * i))));
    } else {
        out.extend((0..i4(code, p + 4) as usize).map(|i| rel(i4(code, p + 12 + 8 * i))));
    }
    out
}
