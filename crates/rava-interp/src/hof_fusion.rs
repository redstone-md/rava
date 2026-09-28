//! Structural higher-order fusion planning.
//!
//! This module never recognizes workload identities. A plan is admitted only from
//! an `invokedynamic` LambdaMetafactory bootstrap and its static implementation
//! handle, plus bytecode, descriptors, CFG, and resolved-target proofs. Missing or
//! ambiguous evidence returns `None`, so normal allocation and dispatch fail closed.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use rava_classfile::{Code, ConstantPool};

use crate::{jit, LoadedClass, Vm};

pub(crate) struct HofFusionPlan {
    pub indy_pc: usize,
    pub consumer_pc: usize,
    pub capture_count: usize,
    pub sam_call_pc: usize,
    pub consumer: jit::InlineCallee,
    pub implementation: jit::InlineCallee,
}

/// Adjacent indy→static pairs the scan considered, and plans it actually admitted. The ratio
/// is the coverage this flag's wall A/B is really measuring.
pub(crate) static SITES_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static PLANS_BUILT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_JIT_HOF_FUSION").ok().as_deref() == Some("1"))
}

impl Vm {
    pub(crate) fn build_hof_fusions(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
        descriptor: &str,
        is_static: bool,
    ) -> HashMap<usize, HofFusionPlan> {
        let caller_unsynchronized = class
            .file
            .methods
            .iter()
            .find(|method| {
                method
                    .code
                    .as_ref()
                    .is_some_and(|method_code| std::ptr::eq(method_code, code))
            })
            .is_some_and(|method| method.access_flags & 0x0020 == 0);
        if !enabled() || !code.exceptions.is_empty() || !caller_unsynchronized {
            return HashMap::new();
        }
        let Some(leaders) = jit::find_leaders(&code.bytecode) else {
            return HashMap::new();
        };
        let mut out = HashMap::new();
        let mut pc = 0usize;
        while pc < code.bytecode.len() {
            let Some(len) = jit::insn_len_at(&code.bytecode, pc) else {
                return HashMap::new();
            };
            if code.bytecode[pc] == 0xba {
                let consumer_pc = pc + len;
                // Adjacent is not enough: a branch target would let control enter the
                // skipped consumer without executing the fused producer.
                if !leaders.contains(&consumer_pc) && code.bytecode.get(consumer_pc) == Some(&0xb8)
                {
                    // COVERAGE, because an A/B of this flag measures its ADMISSION, not the
                    // idea. The plan is derived from classfile bootstrap facts plus bytecode,
                    // descriptor, CFG, and exact-target proofs — never prior execution history.
                    // Only a count separates "fusion does not pay" from "nothing was fused".
                    let seen = SITES_SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if seen < 6 {
                        eprintln!(
                            "[HOF-SITE] #{seen} {}#{} pc={pc} (indy→invokestatic adjacency)",
                            class.file.this_class, descriptor
                        );
                    }
                    if let Some(plan) = self.try_hof_fusion(code, class, pc, consumer_pc) {
                        let n = PLANS_BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if n < 12 {
                            eprintln!(
                                "[HOF-FUSION] plan #{n} {}#{} pc={pc} captures={}",
                                class.file.this_class, descriptor, plan.capture_count
                            );
                        }
                        out.insert(pc, plan);
                    }
                }
            }
            pc += len;
        }
        let pairs = out
            .iter()
            .map(|(&indy_pc, plan)| (indy_pc, plan.consumer_pc))
            .collect();
        if caller_has_zero_gc_capability(
            &code.bytecode,
            descriptor,
            is_static,
            code.exceptions.is_empty(),
            &pairs,
        ) {
            out
        } else {
            HashMap::new()
        }
    }

    fn try_hof_fusion(
        &mut self,
        caller_code: &Code,
        caller: &Arc<LoadedClass>,
        indy_pc: usize,
        consumer_pc: usize,
    ) -> Option<HofFusionPlan> {
        let bc = &caller_code.bytecode;
        let indy_cp = u16_at(bc, indy_pc + 1)?;
        let consumer_cp = u16_at(bc, consumer_pc + 1)?;
        let pool = &caller.file.pool;
        let (bootstrap_index, indy_name, indy_desc) = pool.invoke_dynamic(indy_cp)?;
        let bootstrap = caller
            .file
            .bootstrap_methods
            .get(bootstrap_index as usize)?;
        if pool.method_handle(bootstrap.handle_index)?.owner
            != "java/lang/invoke/LambdaMetafactory"
        {
            return None;
        }
        let implementation_handle = *bootstrap.args.get(1)?;
        let (kind, implementation_site) = pool.method_handle_full(implementation_handle)?;
        if kind != 6 {
            return None; // exact static implementation, read from classfile bootstrap facts
        }

        let (capture_types, fi_type) = descriptor_parts(indy_desc)?;
        if capture_types.iter().any(|t| !stable_capture_type(t)) {
            return None;
        }
        let fi_owner = object_type_name(&fi_type)?;

        let consumer_ref = pool.member_ref(consumer_cp)?;
        let consumer_params = descriptor_params(consumer_ref.descriptor)?;
        if consumer_params.last()? != &fi_type
            || consumer_params.iter().any(|t| !one_slot(t))
            || consumer_params[..consumer_params.len() - 1]
                .iter()
                .any(|t| is_ref_type(t))
        {
            return None; // lambda must be the last logical argument; no slot adaptation
        }
        if !primitive_type(descriptor_return(consumer_ref.descriptor)?) {
            return None;
        }
        let sam_local = (consumer_params.len() - 1) as u16;

        let consumer_decl = self
            .resolve_method(
                &consumer_ref.owner,
                &consumer_ref.name,
                &consumer_ref.descriptor,
            )
            .ok()??;
        if !consumer_decl.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let consumer_method = consumer_decl
            .file
            .method(&consumer_ref.name, &consumer_ref.descriptor)?;
        if consumer_method.access_flags & (0x0008 | 0x0020) != 0x0008 {
            return None;
        }
        let consumer_code = consumer_method.code.as_ref()?;
        let (sam_call_pc, sam_desc) = single_sam_call(
            &consumer_code.bytecode,
            sam_local,
            fi_owner,
            indy_name,
            &consumer_decl.file.pool,
        )?;

        let sam_params = descriptor_params(&sam_desc)?;
        if sam_params.len() != 1 || !one_slot_primitive(&sam_params[0]) {
            return None; // tracer 1: one narrow SAM argument
        }
        let sam_return = descriptor_return(&sam_desc)?;
        if !primitive_type(sam_return) {
            return None;
        }
        let impl_desc = implementation_site.descriptor;
        let impl_params = descriptor_params(impl_desc)?;
        let mut exact_params = capture_types.clone();
        exact_params.extend(sam_params);
        if impl_params != exact_params || descriptor_return(impl_desc)? != sam_return {
            return None; // no bridges, boxing, casts, or method-type adaptation
        }

        let max_frame = caller_code.max_locals as usize + caller_code.max_stack as usize;
        // Keep every nested inline frame within the root caller's registered cell
        // capacity. This remains a conservative structural bound even though the
        // HOF fast path itself no longer spills the whole frame.
        let impl_decl = self
            .resolve_method(
                implementation_site.owner,
                implementation_site.name,
                impl_desc,
            )
            .ok()??;
        let impl_method = impl_decl
            .file
            .method(implementation_site.name, impl_desc)?;
        if !impl_decl.clinit_done.load(std::sync::atomic::Ordering::Relaxed)
            || impl_method.access_flags & (0x0008 | 0x0020) != 0x0008
        {
            return None;
        }
        let impl_code = impl_method.code.as_ref()?;
        if !jit::is_hof_implementation_inlinable(impl_code, impl_desc) {
            return None;
        }
        if impl_code.max_locals as usize + impl_code.max_stack as usize > max_frame {
            return None;
        }

        let consumer = self.inline_target_for_exact(
            caller,
            consumer_ref.owner,
            consumer_ref.name,
            consumer_ref.descriptor,
            true,
            false,
            0,
            max_frame,
            Some(sam_call_pc),
            false,
        )?;
        let implementation = self.inline_target_for_exact(
            caller,
            implementation_site.owner,
            implementation_site.name,
            impl_desc,
            true,
            false,
            0,
            max_frame,
            None,
            false,
        )?;
        Some(HofFusionPlan {
            indy_pc,
            consumer_pc,
            capture_count: capture_types.len(),
            sam_call_pc,
            consumer,
            implementation,
        })
    }
}

fn single_sam_call(
    bc: &[u8],
    sam_local: u16,
    fi_owner: &str,
    indy_name: &str,
    pool: &ConstantPool,
) -> Option<(usize, String)> {
    let leaders = jit::find_leaders(bc)?;
    let mut match_site = None;
    let mut pc = 0usize;
    while pc < bc.len() {
        let len = jit::insn_len_at(bc, pc)?;
        if astore_local(bc, pc) == Some(sam_local) {
            return None;
        }
        if aload_local(bc, pc) == Some(sam_local) {
            if match_site.is_some() {
                return None;
            }
            let arg_pc = pc + len;
            let arg_len = narrow_arg_load_len(bc, arg_pc)?;
            let call_pc = arg_pc + arg_len;
            if leaders.contains(&arg_pc)
                || leaders.contains(&call_pc)
                || bc.get(call_pc) != Some(&0xb9)
            {
                return None;
            }
            let member = pool.member_ref(u16_at(bc, call_pc + 1)?)?;
            if member.owner != fi_owner || member.name != indy_name {
                return None;
            }
            let params = descriptor_params(&member.descriptor)?;
            if params.len() != 1 || !load_matches_type(bc[arg_pc], &params[0]) {
                return None;
            }
            match_site = Some((call_pc, member.descriptor.to_string()));
        }
        pc += len;
    }
    match_site
}

fn descriptor_parts(desc: &str) -> Option<(Vec<String>, String)> {
    Some((
        descriptor_params(desc)?,
        descriptor_return(desc)?.to_string(),
    ))
}

fn descriptor_params(desc: &str) -> Option<Vec<String>> {
    let start = desc.find('(')? + 1;
    let end = desc[start..].find(')')? + start;
    let bytes = desc.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        let p = i;
        while bytes.get(i) == Some(&b'[') {
            i += 1;
        }
        if bytes.get(i) == Some(&b'L') {
            while i < end && bytes[i] != b';' {
                i += 1;
            }
            if i >= end {
                return None;
            }
        }
        i += 1;
        out.push(desc[p..i].to_string());
    }
    Some(out)
}

fn descriptor_return(desc: &str) -> Option<&str> {
    let end = desc.rfind(')')?;
    desc.get(end + 1..).filter(|r| !r.is_empty())
}

fn object_type_name(desc: &str) -> Option<&str> {
    desc.strip_prefix('L')?.strip_suffix(';')
}

fn one_slot(desc: &str) -> bool {
    !matches!(desc.as_bytes().first(), Some(b'J' | b'D'))
}

fn primitive_type(desc: &str) -> bool {
    matches!(
        desc.as_bytes(),
        [b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D']
    )
}

fn one_slot_primitive(desc: &str) -> bool {
    primitive_type(desc) && one_slot(desc)
}

fn stable_capture_type(desc: &str) -> bool {
    one_slot_primitive(desc)
        || matches!(
            desc.as_bytes(),
            [b'[', b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D']
        )
}

fn is_ref_type(desc: &str) -> bool {
    matches!(desc.as_bytes().first(), Some(b'L' | b'['))
}

fn u16_at(bc: &[u8], at: usize) -> Option<u16> {
    Some(((*bc.get(at)? as u16) << 8) | *bc.get(at + 1)? as u16)
}

fn aload_local(bc: &[u8], pc: usize) -> Option<u16> {
    match *bc.get(pc)? {
        0x19 => Some(*bc.get(pc + 1)? as u16),
        op @ 0x2a..=0x2d => Some((op - 0x2a) as u16),
        _ => None,
    }
}

fn astore_local(bc: &[u8], pc: usize) -> Option<u16> {
    match *bc.get(pc)? {
        0x3a => Some(*bc.get(pc + 1)? as u16),
        op @ 0x4b..=0x4e => Some((op - 0x4b) as u16),
        _ => None,
    }
}

fn narrow_arg_load_len(bc: &[u8], pc: usize) -> Option<usize> {
    match *bc.get(pc)? {
        0x15 | 0x17 | 0x19 => Some(2),
        0x1a..=0x1d | 0x22..=0x25 | 0x2a..=0x2d => Some(1),
        _ => None,
    }
}

fn load_matches_type(op: u8, desc: &str) -> bool {
    match desc.as_bytes().first() {
        Some(b'F') => matches!(op, 0x17 | 0x22..=0x25),
        Some(b'L' | b'[') => matches!(op, 0x19 | 0x2a..=0x2d),
        Some(b'I' | b'Z' | b'B' | b'C' | b'S') => matches!(op, 0x15 | 0x1a..=0x1d),
        _ => false,
    }
}

/// Proves that a fused caller cannot return from a moving-GC operation. Primitive
/// arrays may enter as arguments and may fault, but a fault unwinds the method;
/// successful execution contains only the exact fused call pairs and therefore
/// cannot collect. Missing or unclassified bytecode fails closed.
fn caller_has_zero_gc_capability(
    bc: &[u8],
    descriptor: &str,
    is_static: bool,
    exception_free: bool,
    pairs: &HashMap<usize, usize>,
) -> bool {
    if !is_static || !exception_free || pairs.is_empty() {
        return false;
    }
    let Some(params) = descriptor_params(descriptor) else {
        return false;
    };
    if params.iter().any(|p| !stable_capture_type(p)) {
        return false;
    }
    let Some(ret) = descriptor_return(descriptor) else {
        return false;
    };
    if ret != "V" && !primitive_type(ret) {
        return false;
    }

    let consumers: std::collections::HashSet<usize> = pairs.values().copied().collect();
    if consumers.len() != pairs.len() {
        return false;
    }
    let mut seen_indy = 0usize;
    let mut seen_consumer = 0usize;
    let mut pc = 0usize;
    while pc < bc.len() {
        let Some(len) = jit::insn_len_at(bc, pc) else {
            return false;
        };
        match bc[pc] {
            0xba => {
                let Some(&consumer_pc) = pairs.get(&pc) else {
                    return false;
                };
                if consumer_pc != pc + len || bc.get(consumer_pc) != Some(&0xb8) {
                    return false;
                }
                seen_indy += 1;
            }
            0xb8 if consumers.contains(&pc) => seen_consumer += 1,
            0x01
            | 0x12..=0x14
            | 0x32
            | 0x4f..=0x56
            | 0xa5
            | 0xa6
            | 0xa8
            | 0xa9
            | 0xb0
            | 0xb2..=0xb7
            | 0xb8..=0xba
            | 0xbb..=0xbd
            | 0xbf..=0xc9 => return false,
            _ => {}
        }
        pc += len;
    }
    seen_indy == pairs.len() && seen_consumer == pairs.len()
}

#[cfg(test)]
#[path = "hof_fusion_tests.rs"]
mod tests;
