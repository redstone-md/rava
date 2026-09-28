//! Retirement and publication after every worker has joined.

use super::*;

#[cfg(feature = "next-gen-gc")]
struct RootedWrite {
    slot: usize,
    receiver_root: usize,
    value_root: usize,
    layout: field_txn::FieldLayout,
}

#[cfg(feature = "next-gen-gc")]
struct RootedFieldTxn {
    writes: Vec<RootedWrite>,
}

#[cfg(feature = "next-gen-gc")]
impl RootedFieldTxn {
    fn prepare(txn: field_txn::FieldTxn, vm: &mut Vm) -> Option<Self> {
        let mut writes = Vec::with_capacity(txn.writes().len());
        for write in txn.into_writes() {
            if !field_txn::FieldTxn::admissible(write.receiver) {
                return None;
            }
            let (slot, kind) = crate::nextgen::slot_kind_of(
                crate::nextgen::arena_class_id(write.receiver),
                write.field_id,
            )?;
            if field_txn::FieldLayout::from_kind(kind) != Some(write.layout) {
                return None;
            }
            encode_transaction_cell(write.layout, &write.value)?;

            let receiver_root = crate::vm::thread_context::root_len(&vm.gc_protect);
            crate::vm::thread_context::push_root(&mut vm.gc_protect, Value::Obj(write.receiver));
            let value_root = crate::vm::thread_context::root_len(&vm.gc_protect);
            crate::vm::thread_context::push_root(&mut vm.gc_protect, write.value);
            writes.push(RootedWrite {
                slot,
                receiver_root,
                value_root,
                layout: write.layout,
            });
        }
        Some(Self { writes })
    }

    fn commit(&self, vm: &mut Vm) {
        for write in &self.writes {
            let receiver = match crate::vm::thread_context::root_index(
                &vm.gc_protect,
                write.receiver_root,
            ) {
                Value::Obj(receiver) => receiver,
                _ => unreachable!("a rooted transaction receiver remains an arena object"),
            };
            let value = crate::vm::thread_context::root_index(&vm.gc_protect, write.value_root);
            let cell = encode_transaction_cell(write.layout, &value)
                .expect("transaction values were preflighted");
            if write.layout == field_txn::FieldLayout::Ref {
                crate::opcodes::store_arena_ref_cell_encoded(receiver, write.slot, cell);
            } else {
                crate::nextgen::arena_set_cell(receiver, write.slot, cell);
            }
        }
    }
}

#[cfg(feature = "next-gen-gc")]
fn encode_transaction_cell(layout: field_txn::FieldLayout, value: &Value) -> Option<i64> {
    use field_txn::FieldLayout::{Ref, Word32, Word64};
    match (layout, value) {
        (Word32, Value::Int(value)) => Some(*value as u32 as i64),
        (Word32, Value::Float(value)) => Some(value.to_bits() as i64),
        (Word64, Value::Long(value)) => Some(*value),
        (Word64, Value::Double(value)) => Some(value.to_bits() as i64),
        (Ref, Value::Null | Value::Obj(_) | Value::Ref(_) | Value::Array(_)) => {
            Some(crate::opcodes::encode_arena_ref_cell(value))
        }
        _ => None,
    }
}
#[cfg(feature = "next-gen-gc")]
fn prepare_region(
    vm: &mut Vm,
    slots: &mut [LaneSlot],
    bits: &[Option<i64>],
) -> Option<(Vec<usize>, Vec<RootedFieldTxn>)> {
    let mut transactions = Vec::with_capacity(slots.len());
    for slot in slots.iter_mut() {
        transactions.push(RootedFieldTxn::prepare(std::mem::take(&mut slot.txn), vm)?);
    }

    let mut result_roots = Vec::with_capacity(slots.len());
    for (slot, bits) in slots.iter().zip(bits) {
        let plan = slot.plan.as_ref()?;
        let value = publish(plan, vm, (*bits)?);
        result_roots.push(crate::vm::thread_context::root_len(&vm.gc_protect));
        crate::vm::thread_context::push_root(&mut vm.gc_protect, value);
    }
    Some((result_roots, transactions))
}

pub(super) struct JoinedRegion {
    slots: Vec<LaneSlot>,
    bits: Vec<Option<i64>>,
    aborted: bool,
}

impl JoinedRegion {
    pub(super) fn new(slots: Vec<LaneSlot>, aborted: bool) -> Self {
        let bits = slots
            .iter()
            .map(|slot| {
                if aborted || slot.plan.is_none() {
                    return None;
                }
                match slot.out {
                    Some((bits, false, _)) => Some(bits),
                    _ => None,
                }
            })
            .collect();
        Self {
            slots,
            bits,
            aborted,
        }
    }

    fn keep(&self) -> bool {
        !self.aborted && self.bits.iter().all(Option::is_some)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn report(
        &self,
        n: usize,
        pilot: usize,
        open: bool,
        claimed: usize,
        pool_len: usize,
        pool_used: usize,
        pool_claims: u64,
        pilot_cbs: u64,
    ) {
        if !rings() {
            return;
        }
        let completed = self.bits.iter().filter(|b| b.is_some()).count();
        let par = self.slots.len();
        let mut cb_min = u64::MAX;
        let mut cb_sum = 0u64;
        for slot in &self.slots {
            let count = slot.out.map(|out| out.2).unwrap_or(0);
            cb_min = cb_min.min(count);
            cb_sum = cb_sum.saturating_add(count);
        }
        let cb_min = if self.slots.is_empty() { 0 } else { cb_min };
        eprintln!(
            "[GILFREE-RINGS] batch n={n} pilot={pilot} region={open} claimed={claimed}/{par} completed={completed}/{par} keep={} aborted={} cbs_min={cb_min} cbs_sum={cb_sum} pilot_cbs={pilot_cbs} slice={}KB pool={}KB pool_used={}KB pool_claims={pool_claims} nursery_full={} t={}ms",
            self.keep(), self.aborted, nursery_slice() / 1024, pool_len / 1024,
            pool_used / 1024, CLAIM_NURSERY_FULL.load(std::sync::atomic::Ordering::Relaxed), now_ms(),
        );
        drain_abort_sites();
    }

    pub(super) fn finish(mut self, vm: &mut Vm, pilot: usize, before: usize, retained: usize) {
        let mut keep = self.keep();
        #[cfg(feature = "next-gen-gc")]
        if keep
            && self.slots.iter().any(|slot| {
                slot.plan
                    .as_ref()
                    .is_some_and(|plan| plan.field_txn)
            })
        {
            let root_base = crate::vm::thread_context::root_len(&vm.gc_protect);
            crate::nextgen::young_set_top(retained);
            if let Some((result_roots, transactions)) =
                prepare_region(vm, &mut self.slots, &self.bits)
            {
                for (k, (result_root, transaction)) in
                    result_roots.into_iter().zip(transactions).enumerate()
                {
                    transaction.commit(vm);
                    let value =
                        crate::vm::thread_context::root_index(&vm.gc_protect, result_root);
                    crate::taskgraph_probe::count_gilfree_done();
                    let future = vm.lane.pending[pilot + k].future.clone();
                    crate::reflect::complete_queued_task(
                        vm,
                        Some(future),
                        None,
                        Ok(Ok(Some(value))),
                    );
                }
                crate::vm::thread_context::truncate_roots(&mut vm.gc_protect, root_base);
                return;
            }
            crate::vm::thread_context::truncate_roots(&mut vm.gc_protect, root_base);
            keep = false;
        }

        // Drop worker journals before retreating: they may contain pointers into
        // the speculative nursery span that serial retry is about to reuse.
        for slot in &mut self.slots {
            slot.txn = field_txn::FieldTxn::default();
        }
        crate::nextgen::young_set_top(if keep { retained } else { before });
        if !keep {
            for slot in &self.slots {
                if let Some(plan) = &slot.plan {
                    vm.heap.release_obj_range(plan.obase, plan.olen);
                }
            }
        }
        for (k, slot) in self.slots.iter().enumerate() {
            let i = pilot + k;
            let future = vm.lane.pending[i].future.clone();
            let outcome = if keep {
                let plan = slot.plan.as_ref().expect("kept task has a plan");
                let value = publish(plan, vm, self.bits[k].expect("kept task has a result"));
                crate::taskgraph_probe::count_gilfree_done();
                Ok(Ok(Some(value)))
            } else {
                crate::taskgraph_probe::count_gilfree_abort();
                let task = vm.lane.pending[i].task.clone();
                let _pilot = crate::pilot_census::enabled()
                    .then(|| crate::taskgraph_probe::owner_of(vm, &task))
                    .and_then(crate::pilot_census::task_scope);
                let args = vm.lane.pending[i].args.clone();
                // Ordinary execution performs lazy class initialization in guest
                Ok(vm.apply(&task, args))
            };
            crate::reflect::complete_queued_task(vm, Some(future), None, outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(result: Option<(i64, bool, u64)>) -> LaneSlot {
        LaneSlot {
            plan: Some(GilfreeCfPlan {
                entry: 0,
                cells: Vec::new(),
                ret: None,
                ret_desc: "V".into(),
                nbase: 0,
                nlen: 0,
                obase: 0,
                olen: 0,
                fbbase: 0,
                fblen: 0,
                field_txn: false,
            }),
            out: result,
            txn: field_txn::FieldTxn::default(),
        }
    }

    #[test]
    fn heterogeneous_completed_tasks_are_kept_without_a_work_floor() {
        let region = JoinedRegion::new(
            vec![
                slot(Some((11, false, 5_600_000))),
                slot(Some((22, false, 0))),
                slot(Some((33, false, 1))),
            ],
            false,
        );
        assert!(region.keep());
        assert_eq!(region.bits, vec![Some(11), Some(22), Some(33)]);
    }

    #[test]
    fn late_region_abort_invalidates_already_completed_siblings() {
        let region = JoinedRegion::new(
            vec![
                slot(Some((11, false, u64::MAX))),
                slot(Some((22, false, u64::MAX))),
            ],
            true,
        );
        assert!(region.bits.iter().all(Option::is_none));
    }

    #[test]
    fn absent_panicking_or_aborted_task_discards_the_whole_region() {
        for result in [None, Some((0, true, 0)), Some((99, true, u64::MAX))] {
            let region = JoinedRegion::new(vec![slot(Some((11, false, 1))), slot(result)], false);
            assert!(!region.keep());
        }
        let region = JoinedRegion::new(
            vec![LaneSlot {
                plan: None,
                out: Some((11, false, 7)),
                txn: field_txn::FieldTxn::default(),
            }],
            false,
        );
        assert!(!region.keep());
    }

    #[test]
    fn refused_class_initializes_only_on_serial_execution_after_retirement() {
        use rava_classfile::{ClassFile, Code, Constant, ConstantPool, MemberInfo};
        use std::sync::atomic::Ordering::Relaxed;
        let _guard = crate::parallel::TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut vm = Vm::new(crate::class_loader::ClassLoader::new());
        let pool = ConstantPool::from_entries(vec![
            Constant::Unusable,
            Constant::Utf8("DeferredInit".into()),
            Constant::Class { name_index: 1 },
            Constant::Utf8("value".into()),
            Constant::Utf8("I".into()),
            Constant::NameAndType {
                name_index: 3,
                desc_index: 4,
            },
            Constant::FieldRef {
                class_index: 2,
                nat_index: 5,
            },
        ]);
        let class = vm.loader.define_synthetic(ClassFile::synthesize(
            pool,
            0x0021,
            "DeferredInit".into(),
            None,
            Vec::new(),
            vec![MemberInfo {
                access_flags: 0x0008,
                name: "<clinit>".into(),
                descriptor: "()V".into(),
                code: Some(Code {
                    max_stack: 1,
                    max_locals: 0,
                    bytecode: vec![0x10, 7, 0xb3, 0, 6, 0xb1],
                    ..Default::default()
                }),
            }],
        ));
        let before = crate::nextgen::young_extent().1;
        crate::nextgen::young_advance_top(1024);
        let reserved = crate::nextgen::young_extent().1;
        crate::parallel::clear_abort();
        crate::parallel::set_gilfree(true);
        let result = vm.ensure_init(&class);
        crate::parallel::set_gilfree(false);
        assert!(result.is_ok());
        assert!(crate::parallel::abort_requested());
        assert!(!class.initialized.load(Relaxed));
        let region = JoinedRegion::new(Vec::new(), crate::parallel::abort_requested());
        crate::parallel::clear_abort();
        region.finish(&mut vm, 0, before, reserved);
        assert_eq!(crate::nextgen::young_extent().1, before);
        assert!(!class.initialized.load(Relaxed));
        vm.ensure_init(&class)
            .expect("serial initialization succeeds");
        assert!(class.initialized.load(Relaxed));
        assert!(matches!(
            class.statics.read().unwrap().get("value"),
            Some(Value::Int(7))
        ));
    }
    #[cfg(feature = "next-gen-gc")]
    fn arena_int_receiver(name: &str, field_id: u32) -> crate::arena::ArenaPtr {
        let name: std::sync::Arc<str> = name.into();
        let layout: std::sync::Arc<[(u32, u8)]> = vec![(field_id, 0)].into();
        let class_id = crate::nextgen::register_class(&name, &layout);
        crate::nextgen::tlab_try_alloc(class_id, 1).expect("arena object allocation")
    }

    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn prepared_commit_uses_forwardable_root_values() {
        let mut vm = Vm::new(crate::class_loader::ClassLoader::new());
        let receiver = arena_int_receiver("FieldTxnCommitTest", 7);
        let mut txn = field_txn::FieldTxn::new();
        txn.push(receiver, 7, field_txn::FieldLayout::Word32, Value::Int(11));

        let rooted = RootedFieldTxn::prepare(txn, &mut vm).expect("valid journal");
        let value_root = rooted.writes[0].value_root;
        crate::vm::thread_context::root_replace(&mut vm.gc_protect, value_root, Value::Int(29));
        rooted.commit(&mut vm);
        assert!(matches!(
            crate::opcodes::arena_getfield(receiver, 7),
            Value::Int(29)
        ));
    }

    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn ordered_task_commits_expose_each_shared_prefix() {
        let mut vm = Vm::new(crate::class_loader::ClassLoader::new());
        let receiver = arena_int_receiver("FieldTxnOrderTest", 8);
        let mut first = field_txn::FieldTxn::new();
        first.push(receiver, 8, field_txn::FieldLayout::Word32, Value::Int(11));
        let mut second = field_txn::FieldTxn::new();
        second.push(receiver, 8, field_txn::FieldLayout::Word32, Value::Int(22));

        let first = RootedFieldTxn::prepare(first, &mut vm).expect("first journal");
        let second = RootedFieldTxn::prepare(second, &mut vm).expect("second journal");
        first.commit(&mut vm);
        assert!(matches!(
            crate::opcodes::arena_getfield(receiver, 8),
            Value::Int(11)
        ));
        second.commit(&mut vm);
        assert!(matches!(
            crate::opcodes::arena_getfield(receiver, 8),
            Value::Int(22)
        ));
    }

    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn moving_collection_forwards_journal_receivers_and_values() {
        let _guard = crate::parallel::TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let receiver_class = crate::nextgen::register_class(
            &std::sync::Arc::<str>::from("FieldTxnGcReceiver"),
            &std::sync::Arc::<[(u32, u8)]>::from(vec![(17, 6)]),
        );
        let child_class = crate::nextgen::register_class(
            &std::sync::Arc::<str>::from("FieldTxnGcChild"),
            &std::sync::Arc::<[(u32, u8)]>::from(vec![(18, 0)]),
        );
        let receiver = crate::nextgen::tlab_try_alloc(receiver_class, 1).expect("receiver");
        let child = crate::nextgen::tlab_try_alloc(child_class, 1).expect("child");
        unsafe { crate::nextgen::set_cell(child.as_ptr(), 0, 73) };
        let old_receiver = receiver.as_ptr() as usize;
        let old_child = child.as_ptr() as usize;
        let mut vm = Vm::new(crate::class_loader::ClassLoader::new());
        let mut txn = field_txn::FieldTxn::new();
        txn.push(receiver, 17, field_txn::FieldLayout::Ref, Value::Obj(child));

        let rooted = RootedFieldTxn::prepare(txn, &mut vm).expect("valid journal");
        let receiver_root = rooted.writes[0].receiver_root;
        let value_root = rooted.writes[0].value_root;
        vm.arena_collect_jit();

        let forwarded_receiver = match crate::vm::thread_context::root_get(
            &vm.gc_protect,
            receiver_root,
        ) {
            Some(Value::Obj(pointer)) => pointer,
            other => panic!("receiver root changed to {other:?}"),
        };
        let forwarded_child =
            match crate::vm::thread_context::root_get(&vm.gc_protect, value_root) {
                Some(Value::Obj(pointer)) => pointer,
                other => panic!("value root changed to {other:?}"),
            };
        assert_ne!(forwarded_receiver.as_ptr() as usize, old_receiver);
        assert_ne!(forwarded_child.as_ptr() as usize, old_child);

        rooted.commit(&mut vm);
        assert!(matches!(
            crate::opcodes::arena_getfield(forwarded_receiver, 17),
            Value::Obj(pointer) if pointer == forwarded_child
        ));
        assert_eq!(unsafe { crate::nextgen::cell(forwarded_child.as_ptr(), 0) }, 73);
    }

    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn failed_preflight_leaves_every_shared_cell_unmodified() {
        let mut vm = Vm::new(crate::class_loader::ClassLoader::new());
        let receiver = arena_int_receiver("FieldTxnPreflightTest", 9);
        let mut foreign_storage = [0u8; 16];
        let foreign = crate::arena::ArenaPtr::new(foreign_storage.as_mut_ptr())
            .expect("stack pointer is non-null");
        let mut txn = field_txn::FieldTxn::new();
        txn.push(receiver, 9, field_txn::FieldLayout::Word32, Value::Int(41));
        txn.push(foreign, 9, field_txn::FieldLayout::Word32, Value::Int(99));

        let root_base = crate::vm::thread_context::root_len(&vm.gc_protect);
        assert!(RootedFieldTxn::prepare(txn, &mut vm).is_none());
        crate::vm::thread_context::truncate_roots(&mut vm.gc_protect, root_base);
        assert!(matches!(
            crate::opcodes::arena_getfield(receiver, 9),
            Value::Int(0)
        ));
    }
}
