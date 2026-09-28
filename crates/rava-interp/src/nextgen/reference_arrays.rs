//! Exact descriptor metadata for arena reference arrays.
//!
//! Registration is the only place where a synthetic registry name is formed. Runtime routing
//! uses the immutable CID stored in the object header, and the allocation helper delegates to the
//! pure TLAB bump path: a full nursery returns `None` and does not collect.

use crate::arena::ArenaPtr;
use crate::nextgen::*;
use std::sync::Arc;

/// Register one exact component descriptor and return its immutable arena class id.
pub fn register_ref_array_class(component: &str) -> u32 {
    // The registry name is diagnostics-only. All runtime checks use the CID's retained component
    // metadata, so a workload/class name cannot affect eligibility or dispatch.
    let descriptor = format!("[arena-ref-array:{component}]");
    register_ref_array_class_with_descriptor(&descriptor, component)
}

/// Read the exact component descriptor retained by the array object's class CID.
pub fn arena_ref_array_component(p: ArenaPtr) -> Option<Arc<str>> {
    // `is_arena_obj_addr` proves the header read itself is mapped and aligned. The mirror lookup
    // then validates the CID without taking the registry mutex on every aaload/aastore callback.
    // Exact descriptor-bearing CIDs are the representation proof.
    if !is_arena_obj_addr(p.addr()) {
        return None;
    }
    ref_array_component_descriptor(arena_class_id(p))
}

/// Structural predicate for an arena reference-array object.
#[inline]
pub fn is_arena_ref_array(p: ArenaPtr) -> bool {
    arena_ref_array_component(p).is_some()
}

/// Allocate an exact reference-array class instance through the TLAB-only path.
///
/// This helper does not run collection. `None` means the underlying nursery/worker view is full;
/// the caller owns the normal retry or safepoint decision.
pub fn tlab_alloc_ref_array(component: &str, len: u32) -> Option<ArenaPtr> {
    let class_id = register_ref_array_class(component);
    debug_assert!(is_ref_array_class(class_id));
    tlab_try_alloc(class_id, len)
}
