//! Test-only, offline collection of direct bytecode effects.
//!
//! This intentionally does not make an offloadability decision. A `Scanned` body can still be
//! unsafe: field ownership, call summaries, class initialization order, and bootstrap behavior are
//! unresolved. `proven_parallel_safe` therefore remains false until those proofs exist.

use crate::jit::insn_len_at;
use rava_classfile::{Code, ConstantPool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldAccess {
    StaticRead,
    StaticWrite,
    InstanceRead,
    InstanceWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dispatch {
    Static,
    Special,
    Virtual,
    Interface,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FieldEffect {
    pub access: FieldAccess,
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CallEdge {
    pub dispatch: Dispatch,
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

#[derive(Debug, Default)]
pub(super) struct DirectEffects {
    pub fields: Vec<FieldEffect>,
    pub calls: Vec<CallEdge>,
    pub array_stores: usize,
    pub monitor_ops: usize,
    pub dynamic_calls: usize,
    pub handlers: usize,
    pub unknown_sites: Vec<String>,
}

impl DirectEffects {
    /// Direct scanning is evidence only, never an admission proof.
    pub fn proven_parallel_safe(&self) -> bool {
        false
    }
}

/// Collect direct field accesses and call edges without loading or executing guest classes.
pub(super) fn collect(code: &Code, pool: &ConstantPool) -> DirectEffects {
    let mut result = DirectEffects {
        handlers: code.exceptions.len(),
        ..DirectEffects::default()
    };
    let bytecode = &code.bytecode;
    let mut pc = 0;
    while pc < bytecode.len() {
        let opcode = bytecode[pc];
        let Some(length) = insn_len_at(bytecode, pc) else {
            result.unknown_sites.push(format!("malformed-opcode@{pc}:0x{opcode:02x}"));
            break;
        };
        let Some(next_pc) = pc.checked_add(length).filter(|end| *end <= bytecode.len()) else {
            result.unknown_sites.push(format!("truncated-instruction@{pc}:0x{opcode:02x}"));
            break;
        };
        let cp_index = || {
            bytecode
                .get(pc + 1..pc + 3)
                .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        };
        match opcode {
            0xb2..=0xb5 => {
                let Some(member) = cp_index().and_then(|index| pool.member_ref(index)) else {
                    result.unknown_sites.push(format!("unresolved-field@{pc}"));
                    pc += length;
                    continue;
                };
                let access = match opcode {
                    0xb2 => FieldAccess::StaticRead,
                    0xb3 => FieldAccess::StaticWrite,
                    0xb4 => FieldAccess::InstanceRead,
                    _ => FieldAccess::InstanceWrite,
                };
                result.fields.push(FieldEffect {
                    access,
                    owner: member.owner.to_owned(),
                    name: member.name.to_owned(),
                    descriptor: member.descriptor.to_owned(),
                });
            }
            0xb6..=0xb9 => {
                let Some(member) = cp_index().and_then(|index| pool.member_ref(index)) else {
                    result.unknown_sites.push(format!("unresolved-call@{pc}"));
                    pc += length;
                    continue;
                };
                let dispatch = match opcode {
                    0xb6 => Dispatch::Virtual,
                    0xb7 => Dispatch::Special,
                    0xb8 => Dispatch::Static,
                    _ => Dispatch::Interface,
                };
                result.calls.push(CallEdge {
                    dispatch,
                    owner: member.owner.to_owned(),
                    name: member.name.to_owned(),
                    descriptor: member.descriptor.to_owned(),
                });
            }
            0xba => {
                result.dynamic_calls += 1;
                result.unknown_sites.push(format!("invokedynamic@{pc}"));
            }
            0x4f..=0x56 => result.array_stores += 1,
            0xc2 | 0xc3 => result.monitor_ops += 1,
            _ => {}
        }
        pc = next_pc;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rava_classfile::{ClassFile, Constant};
    use rava_jar::Jar;

    fn pool() -> ConstantPool {
        ConstantPool::from_entries(vec![
            Constant::Unusable,
            Constant::Class { name_index: 2 },
            Constant::Utf8("C".into()),
            Constant::NameAndType { name_index: 4, desc_index: 5 },
            Constant::Utf8("field".into()),
            Constant::Utf8("I".into()),
            Constant::FieldRef { class_index: 1, nat_index: 3 },
            Constant::NameAndType { name_index: 8, desc_index: 9 },
            Constant::Utf8("call".into()),
            Constant::Utf8("()V".into()),
            Constant::MethodRef { class_index: 1, nat_index: 7 },
        ])
    }

    #[test]
    fn collects_all_direct_field_writes_and_call_edges_but_never_proves_safety() {
        let code = Code { bytecode: vec![0xb3, 0, 6, 0xb5, 0, 6, 0xb8, 0, 10, 0xb1], ..Code::default() };
        let effects = collect(&code, &pool());
        assert_eq!(effects.fields.len(), 2);
        assert_eq!(effects.fields[0].access, FieldAccess::StaticWrite);
        assert_eq!(effects.fields[1].access, FieldAccess::InstanceWrite);
        assert_eq!(effects.calls.len(), 1);
        assert_eq!(effects.calls[0].dispatch, Dispatch::Static);
        assert!(!effects.proven_parallel_safe());
    }

    #[test]
    fn malformed_and_dynamic_sites_fail_closed_without_suppressing_other_facts() {
        let code = Code { bytecode: vec![0xba, 0, 1, 0, 0, 0xb3, 0, 6, 0xff], ..Code::default() };
        let effects = collect(&code, &pool());
        assert_eq!(effects.dynamic_calls, 1);
        assert_eq!(effects.fields.len(), 1);
        assert_eq!(effects.unknown_sites.len(), 2);
        assert!(!effects.proven_parallel_safe());
    }

    #[test]
    #[ignore = "manual offline analysis; set RAVA_EFFECT_SUMMARY_JAR to the server jar"]
    fn reports_blocks_clinit_direct_effect_counts() {
        let jar_path = std::env::var("RAVA_EFFECT_SUMMARY_JAR").expect("set jar path");
        let mut jar = Jar::open(jar_path).expect("open jar");
        let bytes = jar.read_class("net/minecraft/world/level/block/Blocks").expect("read jar entry").expect("Blocks class missing");
        let class = ClassFile::parse(&bytes).expect("parse Blocks class");
        let clinit = class.method("<clinit>", "()V").expect("Blocks.<clinit>").code.as_ref().expect("clinit bytecode");
        let effects = collect(clinit, &class.pool);
        eprintln!("Blocks.<clinit>: {} field effects, {} calls, {} array stores, {} monitor ops, {} dynamic calls, {} handlers, {} unknown sites; proven_safe={}",
            effects.fields.len(), effects.calls.len(), effects.array_stores, effects.monitor_ops,
            effects.dynamic_calls, effects.handlers, effects.unknown_sites.len(), effects.proven_parallel_safe());
        assert!(!effects.proven_parallel_safe());
    }
}
