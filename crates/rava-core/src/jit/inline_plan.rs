//! Inlining decisions, made at each call site as the body is emitted (so targets that only
//! become known through constants, such as method-handle chains, can be inlined too).
//!
//! A callee is inlined when it is small: HotSpot's C1 limit of 35 bytecodes, or any size up to a
//! cap for `@ForceInline`. It must not be `@DontInline`, recursive, synchronized or have exception
//! handlers. Two strategies (`inline.rs`):
//! - **Leaf**: a side-effect-free body with no calls or loops runs in a scratch area; any slow
//!   path restarts the call as an ordinary one.
//! - **Framed**: any other small body runs in a real interpreter frame placed at the arguments,
//!   as a call would place it, without a native call.

use super::analysis::{i2, i4, insn_len};
use super::intrinsics::intrinsic;
use crate::classfile::Cp;
use crate::klass::ACC_FINAL;
use crate::link::Resolved;
use crate::method::Method;
use std::sync::atomic::Ordering;

/// HotSpot's `MaxInlineSize` / `C1MaxInlineSize`.
const MAX_INLINE_SIZE: usize = 35;
/// Leaves up to this size are inlined once the callee is hot (cf. `FreqInlineSize`).
const HOT_LEAF_SIZE: usize = 128;
const HOT_CALLEE: u32 = 1000;
/// Scratch slots one leaf body may use.
const MAX_LEAF_SLOTS: usize = 48;
/// Nesting of inlined frames (cf. `C1MaxInlineLevel`, and C2's `MaxInlineLevel` for the
/// optimizing tier).
const MAX_DEPTH: usize = 9;
const MAX_DEPTH_OPT: usize = 15;
/// `@ForceInline` bodies above this size are still left as calls.
const MAX_FORCED_SIZE: usize = 400;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Leaf,
    Framed,
}

/// The bodies being emitted, outermost first, with their first argument when constant.
pub(super) type Chain = [(&'static Method, Option<i64>)];

/// Hot callees inlined at tier 2 up to HotSpot's `FreqInlineSize`, within a per-compilation
/// budget of inlined bytecode.
const HOT_INLINE_SIZE: usize = 325;
const HOT_INVOCATIONS: u32 = 2000;
const TIER2_BUDGET: usize = 6000;
/// A callee whose optimized code is larger than this is called rather than inlined again
/// (HotSpot's `InlineSmallCode`): its own compilation already inlined what it calls.
const INLINE_SMALL_CODE: usize = 3000;
const TIER1_BUDGET: usize = 2000;

/// How to inline `target` (first argument `first`) into `chain` at `tier` (1 and 2 baseline, 3
/// optimized), having inlined `spent` bytes of bytecode already, if at all.
pub(super) fn decide(target: &'static Method, first: Option<i64>, chain: &Chain, tier: u8, spent: usize) -> Option<Kind> {
    let size = target.code.map_or(0, |c| c.code.len());
    if spent + size > if tier >= 2 { TIER2_BUDGET } else { TIER1_BUDGET } {
        return None;
    }
    if std::env::var_os("RAVA_CORE_NO_INLINE").is_some() || target.is_abstract() || target.is_native() || intrinsic(target).is_some() {
        return None;
    }
    if tier >= 3 && target.compiled().is_some_and(|c| c.tier >= 2 && c.size > INLINE_SMALL_CODE) {
        return None;
    }
    if is_leaf(target) {
        return Some(Kind::Leaf);
    }
    // Tier 3 (the optimizing tier) compiles a method because it is hot, so its call sites are:
    // counting the callee's own invocations misses calls inlined into baseline code.
    let hot = tier >= 3 || (tier >= 2 && target.invocations.load(Ordering::Relaxed) >= HOT_INVOCATIONS);
    framed_ok(target, first, chain, hot, tier >= 3).then_some(Kind::Framed)
}

/// The statically bound target of the call at `pc`, excluding method-handle invokers.
pub(super) fn bound_target(caller: &'static Method, code: &[u8], pc: usize) -> Option<&'static Method> {
    let index = u16::from_be_bytes([code[pc + 1], code[pc + 2]]);
    let Some(Resolved::Method(m)) = caller.holder.resolved[index as usize].get().copied() else { return None };
    if m.is_signature_polymorphic() {
        return None;
    }
    match code[pc] {
        0xb8 if m.is_static() => Some(m),
        0xb7 if !m.is_static() => Some(crate::interp::special_target(caller.holder, m)),
        0xb6 if !m.is_static() && (m.is_private() || m.access & ACC_FINAL != 0 || m.holder.access & ACC_FINAL != 0) => Some(m),
        _ => None,
    }
}

/// Recursion is refused beyond `allowed` levels (C2's `MaxRecursiveInlineLevel` for the
/// optimizing tier, none otherwise), except for a lambda-form body applied to a different
/// constant method handle (HotSpot bounds lambda-form recursion by receiver identity the same way).
fn recursive(t: &'static Method, first: Option<i64>, chain: &Chain, allowed: usize) -> bool {
    let lambda_form = t.holder.is_hidden() && t.holder.name.as_str().starts_with("java/lang/invoke/LambdaForm$");
    chain.iter().filter(|(c, f)| std::ptr::eq(*c, t) && !(lambda_form && first.is_some() && *f != first)).count() > allowed
}

/// `opt`: for the optimizing tier, whose exceptions leave compiled code by deoptimizing into
/// the interpreter, so inlined bodies may have exception handlers.
fn framed_ok(t: &'static Method, first: Option<i64>, chain: &Chain, hot: bool, opt: bool) -> bool {
    let Some(info) = t.code else { return false };
    let depth = if opt { MAX_DEPTH_OPT } else { MAX_DEPTH };
    if chain.len() > depth || recursive(t, first, chain, opt as usize) || t.is_synchronized() || (!opt && !info.handlers.is_empty()) {
        return false;
    }
    if t.has_annotation("Ljdk/internal/vm/annotation/DontInline;") {
        return false;
    }
    let forced = t.has_annotation("Ljdk/internal/vm/annotation/ForceInline;");
    if info.code.len() > if forced { MAX_FORCED_SIZE } else if hot { HOT_INLINE_SIZE } else { MAX_INLINE_SIZE } {
        return false;
    }
    let mut pc = 0;
    while pc < info.code.len() {
        if matches!(info.code[pc], 0xa8 | 0xa9 | 0xc9) {
            return false;
        }
        pc += insn_len(info.code, pc);
    }
    true
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
