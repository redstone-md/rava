use super::{generate_with_shrinkwrap, shrinkwrap};
use crate::classfile::CodeInfo;
use crate::jit::opt::ir::{BinOp, Graph, Inst, Term, Ty};
use crate::jit::opt::regalloc::{Allocation, Loc};
use crate::klass::{Klass, KlassKind, KlassParts, ACC_STATIC};
use crate::method::Method;
use crate::symbol::Symbol;
use std::collections::HashMap;

fn fixture() -> (Graph, Allocation, &'static Method) {
    let holder = crate::klass::leak(Klass::new(KlassParts {
        name: Symbol::intern("ShrinkwrapCodegenTest"),
        kind: KlassKind::Instance,
        access: 0,
        super_klass: None,
        interfaces: Vec::new(),
        cf: None,
        fields: Vec::new(),
        instance_size: 0,
        ref_offsets: Vec::new(),
        static_slots: 0,
        initialized: true,
        hidden: false,
    }));
    let code = Box::leak(Box::new(CodeInfo {
        max_stack: 2,
        max_locals: 0,
        code: &[],
        handlers: Vec::new(),
        line_numbers: None,
    }));
    let method = crate::low::leak(Method::new(
        holder,
        Symbol::intern("returnValue"),
        Symbol::intern("()I"),
        ACC_STATIC,
        Some(code),
        false,
    ));

    let mut graph = Graph::default();
    let entry = graph.new_block(0);
    let returned = graph.new_block(1);
    let x = graph.add(returned, Inst::Const(20), Ty::I);
    let y = graph.add(returned, Inst::Const(22), Ty::I);
    let result = graph.add(returned, Inst::Bin(BinOp::Add, x, y), Ty::I);
    graph.blocks[entry as usize].term = Term::Goto(returned);
    graph.blocks[returned as usize].preds.push(entry);
    graph.blocks[returned as usize].term = Term::Return(Some(result));

    let mut loc = vec![Loc::None; graph.values.len()];
    loc[x as usize] = Loc::Const(20);
    loc[y as usize] = Loc::Const(22);
    loc[result as usize] = Loc::Reg(crate::jit::asm::R::Rbx);
    let allocation = Allocation {
        loc,
        order: vec![entry, returned],
        spill_slots: 2,
        saves: HashMap::new(),
        state_saves: HashMap::new(),
        save_base: 2,
        live_at: HashMap::new(),
        live_at_end: vec![(0, 0); graph.blocks.len()],
        home: vec![None; graph.values.len()],
        state_regs: vec![false; graph.values.len()],
    };
    (graph, allocation, method)
}

fn contains(code: &[u8], pattern: &[u8]) -> bool {
    code.windows(pattern.len()).any(|bytes| bytes == pattern)
}

fn count(code: &[u8], pattern: &[u8]) -> usize {
    code.windows(pattern.len())
        .filter(|bytes| *bytes == pattern)
        .count()
}

#[test]
fn selected_plan_is_integrated_into_prologue_and_return_codegen() {
    let (graph, allocation, method) = fixture();
    let plan =
        shrinkwrap::analyze(&graph, &allocation, true).expect("fixture should select a plan");
    assert_eq!(plan.block, 1);
    assert_eq!(plan.reg, crate::jit::asm::R::Rbx);
    assert_eq!(plan.slot, 2);

    let enabled = generate_with_shrinkwrap(&graph, &allocation, method, 0, None, None, false, true)
        .0
        .bytes;
    let disabled =
        generate_with_shrinkwrap(&graph, &allocation, method, 0, None, None, false, false)
            .0
            .bytes;

    let save = [0x48, 0x89, 0x5c, 0x24, 0x10];
    let restore = [0x48, 0x8b, 0x5c, 0x24, 0x10];
    let save_at = enabled
        .windows(save.len())
        .position(|bytes| bytes == save)
        .expect("save is at the selected block entry");
    let restore_at = enabled
        .windows(restore.len())
        .position(|bytes| bytes == restore)
        .expect("restore is before its return");
    let result_load = enabled
        .windows(3)
        .position(|bytes| bytes == [0x48, 0x89, 0xd8])
        .expect("return value is loaded from the selected register");
    assert_eq!(count(&enabled, &save), 1);
    assert_eq!(count(&enabled, &restore), 1);
    assert!(save_at < result_load && result_load < restore_at);
    assert!(
        matches!(enabled.get(restore_at + restore.len()), Some(0xeb | 0xe9)),
        "restore is followed by the return edge"
    );
    assert!(
        enabled.starts_with(&[0x48, 0x83, 0xec]),
        "selected register is not pushed in the prologue"
    );
    assert!(
        !contains(&enabled, &[0x5b, 0xc3]),
        "selected register is not popped in the epilogue"
    );
    assert!(
        disabled.starts_with(&[0x53]),
        "disabled plan keeps the ordinary prologue push"
    );
    assert!(
        contains(&disabled, &[0x5b, 0xc3]),
        "disabled plan keeps the ordinary epilogue pop"
    );
}
