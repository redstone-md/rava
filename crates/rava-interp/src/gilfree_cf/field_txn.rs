//! Per-task ordered field-write buffer for the gil-free lane's private
//! execution context (write-only tracer bullet; see `docs/vm-shared-exec-plan.md`,
//! "publication transactional").
//!
//! A gil-free worker never mutates a shared arena cell directly. Every object
//! field store it executes is instead appended here as a [`FieldWrite`]. The
//! coordinator roots the buffered `Value`s across any moving-GC safepoint (they
//! live in this journal until [`FieldTxn::writes`] / [`FieldTxn::into_writes`]
//! hands them over) and commits them in the ORIGINAL ORDER at the
//! stop-the-world publication boundary, so the worker's write set is applied
//! exactly once and exactly in program order.
//!
//! This module owns only the journal. Admission (which bodies may run
//! lock-free) and the coordinator-side commit are the parent's job: there is no
//! replay, read-back or conflict resolution here, and `record` deliberately
//! refuses anything but a validated live arena object receiver — a pinned
//! mirror (handle object wearing an arena pointer) and a null/foreign address
//! are exactly the shapes whose fields the journal cannot represent honestly.

use crate::arena::ArenaPtr;
use crate::value::Value;

/// How the receiving object's cell for the written field is laid out, so the
/// coordinator commit can pick the matching cell store without re-deriving it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldLayout {
    /// 32-bit primitive cell (int / float / boolean / char / short / byte).
    Word32,
    /// 64-bit primitive cell (long / double).
    Word64,
    /// Reference cell (object / array / string handle or arena pointer).
    Ref,
}

impl FieldLayout {
    /// Map the `u8` slot kind produced by `nextgen`'s layout lookup
    /// (`slot_kind_of`): 0=Int, 1=Long, 2=Float, 3=Double, 6=reference.
    /// Unknown kinds are refused by `record`.
    #[inline]
    pub fn from_kind(kind: u8) -> Option<Self> {
        match kind {
            0 | 2 => Some(FieldLayout::Word32),
            1 | 3 => Some(FieldLayout::Word64),
            6 => Some(FieldLayout::Ref),
            _ => None,
        }
    }
}

/// One buffered field store: receiver, field, layout and the value to commit.
#[derive(Clone, Debug)]
pub struct FieldWrite {
    pub receiver: ArenaPtr,
    pub field_id: u32,
    pub layout: FieldLayout,
    pub value: Value,
}

/// Ordered per-task journal of object field writes, private to one worker task.
#[derive(Debug, Default)]
pub struct FieldTxn {
    writes: Vec<FieldWrite>,
}

impl FieldTxn {
    /// Empty journal.
    #[inline]
    pub fn new() -> Self {
        FieldTxn { writes: Vec::new() }
    }

    /// Is this receiver admissible for journaling: a live arena object address
    /// that is not a pinned mirror. Null can never reach here (`ArenaPtr` is
    /// non-null by construction); a foreign / out-of-range address and a pinned
    /// region address are both refused.
    #[cfg(feature = "next-gen-gc")]
    #[inline]
    pub(crate) fn admissible(recv: ArenaPtr) -> bool {
        let a = recv.addr();
        crate::nextgen::is_arena_obj_addr(a) && !crate::nextgen::is_pinned(a)
    }

    /// Record one field store. Returns `false` — leaving the journal
    /// unchanged — when the receiver is not a validated arena object or is a
    /// pinned mirror, or when the layout kind is unknown.
    #[inline]
    pub fn record(
        &mut self,
        receiver: ArenaPtr,
        field_id: u32,
        layout: FieldLayout,
        value: Value,
    ) -> bool {
        #[cfg(feature = "next-gen-gc")]
        if !Self::admissible(receiver) {
            return false;
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            let _ = (receiver, field_id, layout, value);
            return false;
        }
        #[cfg(feature = "next-gen-gc")]
        self.push(receiver, field_id, layout, value);
        #[cfg(feature = "next-gen-gc")]
        true
    }

    /// Append a write whose receiver the caller has already validated. Kept
    /// private except to tests so the ordering guarantees can be pinned without
    /// touching the global arena high-water mark.
    #[inline]
    pub(crate) fn push(
        &mut self,
        receiver: ArenaPtr,
        field_id: u32,
        layout: FieldLayout,
        value: Value,
    ) {
        self.writes.push(FieldWrite {
            receiver,
            field_id,
            layout,
            value,
        });
    }

    /// The journal in original write order (the coordinator's commit order).
    #[inline]
    pub fn writes(&self) -> &[FieldWrite] {
        &self.writes
    }

    /// Take the journal in original write order, leaving this buffer empty.
    #[inline]
    pub fn into_writes(self) -> Vec<FieldWrite> {
        self.writes
    }

    #[cfg(test)]
    fn ptr_of(buf: &mut [u8; 16]) -> ArenaPtr {
        ArenaPtr::new(buf.as_mut_ptr()).unwrap()
    }
}

thread_local! {
    static ACTIVE: std::cell::RefCell<Option<FieldTxn>> =
        const { std::cell::RefCell::new(None) };
}

/// Start the journal for exactly one lane task; stale TLS state is a hard refusal.
pub(crate) fn begin() -> bool {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        if active.is_some() {
            return false;
        }
        *active = Some(FieldTxn::new());
        true
    })
}

/// Buffer one non-owned arena field store, returning false when no transaction
/// is active or the receiver/layout is outside the proven representation.
pub(crate) fn record(receiver: ArenaPtr, field_id: u32, layout: FieldLayout, value: Value) -> bool {
    ACTIVE.with(|active| {
        active
            .borrow_mut()
            .as_mut()
            .is_some_and(|txn| txn.record(receiver, field_id, layout, value))
    })
}

/// Drain the task-local journal before its worker context is torn down.
pub(crate) fn take() -> Option<FieldTxn> {
    ACTIVE.with(|active| active.borrow_mut().take())
}

/// Whether this worker task is currently running under transactional admission.
pub(crate) fn active() -> bool {
    ACTIVE.with(|active| active.borrow().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    #[test]
    fn writes_preserve_original_order() {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        let mut txn = FieldTxn::new();
        txn.push(
            FieldTxn::ptr_of(&mut a),
            7,
            FieldLayout::Word32,
            Value::Int(1),
        );
        txn.push(
            FieldTxn::ptr_of(&mut b),
            7,
            FieldLayout::Word32,
            Value::Int(2),
        );
        txn.push(FieldTxn::ptr_of(&mut a), 3, FieldLayout::Ref, Value::Null);
        let w = txn.writes();
        assert_eq!(w.len(), 3);
        assert!(matches!(&w[0].value, Value::Int(1)));
        assert_eq!(w[0].field_id, 7);
        assert!(matches!(&w[1].value, Value::Int(2)));
        assert_eq!(w[2].layout, FieldLayout::Ref);
        // into_writes hands over the same order and empties by consumption.
        assert_eq!(txn.into_writes().len(), 3);
    }

    #[test]
    fn duplicate_key_writes_commit_in_program_order() {
        // Two stores to the SAME (receiver, field): the last one must stay last
        // so the coordinator replays them in program order and the final value
        // is the last store's.
        let mut a = [0u8; 16];
        let mut txn = FieldTxn::new();
        txn.push(
            FieldTxn::ptr_of(&mut a),
            5,
            FieldLayout::Word32,
            Value::Int(10),
        );
        txn.push(
            FieldTxn::ptr_of(&mut a),
            5,
            FieldLayout::Word32,
            Value::Int(20),
        );
        txn.push(
            FieldTxn::ptr_of(&mut a),
            5,
            FieldLayout::Word32,
            Value::Int(30),
        );
        let w = txn.into_writes();
        assert_eq!(w.len(), 3);
        assert!(matches!(&w[2].value, Value::Int(30)));
        assert!(matches!(
            (&w[0].value, &w[1].value),
            (Value::Int(10), Value::Int(20))
        ));
    }

    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn record_refuses_pinned_and_foreign_receivers() {
        let mut stack_buf = [0u8; 16];
        let mut txn = FieldTxn::new();
        // A stack address is a non-null pointer but is neither inside the arena
        // range nor 8-aligned-by-the-floor, so the arena-obj predicate refuses
        // it; no GC or global runtime state is touched.
        let foreign = FieldTxn::ptr_of(&mut stack_buf);
        assert!(!FieldTxn::admissible(foreign));
        assert!(!txn.record(foreign, 1, FieldLayout::Word32, Value::Int(9)));
        // `ArenaPtr` cannot hold null, so the null-receiver case is expressed by
        // refusing anything `is_arena_obj_addr` rejects; no pinned region is
        // created and no global runtime state is mutated here.
        assert!(txn.writes().is_empty());
    }
}
