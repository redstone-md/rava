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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TargetFreshness {
    Fresh,
    OwnedThis,
    InductionValue,
    Lambda,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CalleeSummary {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
    pub exact: bool,
    pub receiver_private: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EffectFact {
    Field {
        pc: usize,
        access: FieldAccess,
        field: Option<FieldEffect>,
        target: Option<TargetFreshness>,
    },
    ArrayStore {
        pc: usize,
        target: TargetFreshness,
        index_partitioned: bool,
    },
    Call {
        pc: usize,
        edge: Option<CallEdge>,
        receiver: TargetFreshness,
        query: Option<CalleeSummary>,
    },
    CalleeQuery {
        pc: usize,
        query: CalleeSummary,
    },
    Dynamic {
        pc: usize,
        resolved: bool,
    },
    Throw {
        pc: usize,
        target: TargetFreshness,
    },
}

pub(super) fn target_freshness(value: crate::offload::F) -> TargetFreshness {
    match value {
        crate::offload::F::Fresh(_) => TargetFreshness::Fresh,
        crate::offload::F::This => TargetFreshness::OwnedThis,
        crate::offload::F::Iv => TargetFreshness::InductionValue,
        crate::offload::F::Lambda(_) => TargetFreshness::Lambda,
        crate::offload::F::Shared => TargetFreshness::Shared,
    }
}

pub(super) fn observe_field(
    effects: &mut Option<&mut dyn FnMut(EffectFact)>,
    bc: &[u8],
    pc: usize,
    opcode: u8,
    pool: &ConstantPool,
    target: Option<crate::offload::F>,
) {
    let Some(observer) = effects.as_deref_mut() else {
        return;
    };
    let access = match opcode {
        0xb2 => FieldAccess::StaticRead,
        0xb3 => FieldAccess::StaticWrite,
        0xb4 => FieldAccess::InstanceRead,
        _ => FieldAccess::InstanceWrite,
    };
    let cp = crate::offload::u16be(bc, pc + 1);
    let field = pool.member_ref(cp).map(|member| FieldEffect {
        access,
        owner: member.owner.to_owned(),
        name: member.name.to_owned(),
        descriptor: member.descriptor.to_owned(),
    });
    observer(EffectFact::Field {
        pc,
        access,
        field,
        target: target.map(target_freshness),
    });
}

pub(super) fn observe_call(
    effects: &mut Option<&mut dyn FnMut(EffectFact)>,
    pc: usize,
    opcode: u8,
    member: &rava_classfile::MemberRef<'_>,
    receiver: crate::offload::F,
) {
    let Some(observer) = effects.as_deref_mut() else {
        return;
    };
    let dispatch = match opcode {
        0xb6 => Dispatch::Virtual,
        0xb7 => Dispatch::Special,
        0xb8 => Dispatch::Static,
        _ => Dispatch::Interface,
    };
    observer(EffectFact::Call {
        pc,
        edge: Some(CallEdge {
            dispatch,
            owner: member.owner.to_owned(),
            name: member.name.to_owned(),
            descriptor: member.descriptor.to_owned(),
        }),
        receiver: target_freshness(receiver),
        query: None,
    });
}

pub(super) fn observe_query(
    effects: &mut Option<&mut dyn FnMut(EffectFact)>,
    pc: usize,
    query: crate::offload::CalleeQuery<'_>,
) {
    if let Some(observer) = effects.as_deref_mut() {
        observer(EffectFact::CalleeQuery {
            pc,
            query: CalleeSummary {
                owner: query.owner.to_owned(),
                name: query.name.to_owned(),
                descriptor: query.desc.to_owned(),
                exact: query.exact,
                receiver_private: query.recv_private,
            },
        });
    }
}

pub(super) fn observe_dynamic(
    effects: &mut Option<&mut dyn FnMut(EffectFact)>,
    pc: usize,
    resolved: bool,
) {
    if let Some(observer) = effects.as_deref_mut() {
        observer(EffectFact::Dynamic { pc, resolved });
    }
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
            result
                .unknown_sites
                .push(format!("malformed-opcode@{pc}:0x{opcode:02x}"));
            break;
        };
        let Some(next_pc) = pc.checked_add(length).filter(|end| *end <= bytecode.len()) else {
            result
                .unknown_sites
                .push(format!("truncated-instruction@{pc}:0x{opcode:02x}"));
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

#[derive(Debug, Default)]
pub(super) struct FreshnessReport {
    pub facts: Vec<EffectFact>,
    /// False whenever the abstract interpreter stopped at a refusal or malformed path.
    pub body_walk_complete: bool,
    pub rejection: Option<String>,
}

impl FreshnessReport {
    /// A local freshness walk does not close transitive effects or initialization order.
    pub fn proven_parallel_safe(&self) -> bool {
        false
    }
}

/// Run the dormant freshness transfer functions as an offline collector.
/// The permissive resolver only lets local freshness facts continue; it is not a callee proof.
pub(super) fn collect_freshness(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[rava_classfile::BootstrapMethod]>,
) -> FreshnessReport {
    let mut report = FreshnessReport::default();
    let result = crate::offload::analyze::analyze_with_effects(
        code,
        pool,
        bsm,
        false,
        false,
        false,
        false,
        None,
        None,
        &mut |_| true,
        &mut |fact| report.facts.push(fact),
    );
    report.body_walk_complete = result.is_ok();
    report.rejection = result.err().map(|reason| reason.to_string());
    report
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
            Constant::NameAndType {
                name_index: 4,
                desc_index: 5,
            },
            Constant::Utf8("field".into()),
            Constant::Utf8("I".into()),
            Constant::FieldRef {
                class_index: 1,
                nat_index: 3,
            },
            Constant::NameAndType {
                name_index: 8,
                desc_index: 9,
            },
            Constant::Utf8("call".into()),
            Constant::Utf8("()V".into()),
            Constant::MethodRef {
                class_index: 1,
                nat_index: 7,
            },
        ])
    }

    #[test]
    fn collects_all_direct_field_writes_and_call_edges_but_never_proves_safety() {
        let code = Code {
            bytecode: vec![0xb3, 0, 6, 0xb5, 0, 6, 0xb8, 0, 10, 0xb1],
            ..Code::default()
        };
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
        let code = Code {
            bytecode: vec![0xba, 0, 1, 0, 0, 0xb3, 0, 6, 0xff],
            ..Code::default()
        };
        let effects = collect(&code, &pool());
        assert_eq!(effects.dynamic_calls, 1);
        assert_eq!(effects.fields.len(), 1);
        assert_eq!(effects.unknown_sites.len(), 2);
        assert!(!effects.proven_parallel_safe());
    }

    #[test]
    fn freshness_facts_distinguish_private_field_write_and_shared_array_refusal() {
        let code = Code {
            bytecode: vec![0xbb, 0, 1, 0x59, 0x04, 0xb5, 0, 6, 0x57, 0xb1],
            ..Code::default()
        };
        let report = collect_freshness(&code, &pool(), None);
        assert!(report.body_walk_complete, "{:?}", report.rejection);
        assert!(report.facts.iter().any(|fact| matches!(
            fact,
            EffectFact::Field {
                access: FieldAccess::InstanceWrite,
                target: Some(TargetFreshness::Fresh),
                ..
            }
        )));
        assert!(!report.proven_parallel_safe());

        let code = Code {
            max_locals: 1,
            bytecode: vec![0x2a, 0x03, 0x04, 0x4f, 0xb1],
            ..Code::default()
        };
        let report = collect_freshness(&code, &pool(), None);
        assert!(!report.body_walk_complete);
        assert!(report
            .rejection
            .as_deref()
            .is_some_and(|reason| reason.starts_with("shared-write")));
        assert!(matches!(
            report.facts.as_slice(),
            [EffectFact::ArrayStore {
                target: TargetFreshness::Shared,
                index_partitioned: false,
                ..
            }]
        ));
    }

    #[test]
    fn freshness_facts_preserve_exact_callee_query_without_proving_its_body() {
        let code = Code {
            bytecode: vec![0xb8, 0, 10, 0xb1],
            ..Code::default()
        };
        let report = collect_freshness(&code, &pool(), None);
        assert!(report.body_walk_complete, "{:?}", report.rejection);
        assert!(report.facts.iter().any(|fact| matches!(
            fact,
            EffectFact::CalleeQuery { query, .. } if query.owner == "C" && query.name == "call" && query.exact
        )));
        assert!(!report.proven_parallel_safe());
    }

    #[test]
    #[ignore = "manual offline analysis; set RAVA_EFFECT_SUMMARY_JAR to the server jar"]
    fn reports_blocks_clinit_direct_effect_counts() {
        let jar_path = std::env::var("RAVA_EFFECT_SUMMARY_JAR").expect("set jar path");
        let mut jar = Jar::open(jar_path).expect("open jar");
        let bytes = jar
            .read_class("net/minecraft/world/level/block/Blocks")
            .expect("read jar entry")
            .expect("Blocks class missing");
        let class = ClassFile::parse(&bytes).expect("parse Blocks class");
        let clinit = class
            .method("<clinit>", "()V")
            .expect("Blocks.<clinit>")
            .code
            .as_ref()
            .expect("clinit bytecode");
        let effects = collect(clinit, &class.pool);
        eprintln!("Blocks.<clinit>: {} field effects, {} calls, {} array stores, {} monitor ops, {} dynamic calls, {} handlers, {} unknown sites; proven_safe={}",
            effects.fields.len(), effects.calls.len(), effects.array_stores, effects.monitor_ops,
            effects.dynamic_calls, effects.handlers, effects.unknown_sites.len(), effects.proven_parallel_safe());
        assert!(!effects.proven_parallel_safe());
        let freshness = collect_freshness(clinit, &class.pool, Some(&class.bootstrap_methods));
        eprintln!(
            "Blocks.<clinit> freshness prefix: {} facts, complete={}, rejection={:?}, proven_safe={}",
            freshness.facts.len(),
            freshness.body_walk_complete,
            freshness.rejection,
            freshness.proven_parallel_safe(),
        );
        assert!(!freshness.body_walk_complete);
        assert!(freshness.rejection.is_some());
    }
}
