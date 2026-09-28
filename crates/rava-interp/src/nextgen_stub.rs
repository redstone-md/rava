//! Default-build (no `next-gen-gc`) no-op stand-in for the arena layer: exactly the
//! surface the shared (ungated) JIT/diagnostic call sites touch. Without the feature
//! no arena exists and nothing ever moves, so the barrier is identity and every
//! arena probe is empty/false — the default build's behavior is unchanged.

/// Read barrier: no moving collector → nothing is ever forwarded.
pub fn forward_if_moved(cell: i64) -> i64 {
    cell
}
/// No arena → no address is an arena object.
pub fn is_arena_obj_addr(_a: u64) -> bool {
    false
}
/// Never reached with a live arena object (no arena); id 0 is never a real
/// class id (guards use id+1) and `class_name(0)` is `None`, so callers take
/// their general/fallback path.
pub unsafe fn obj_class(_p: *mut u8) -> u32 {
    0
}
pub fn class_name(_class_id: u32) -> Option<std::sync::Arc<str>> {
    None
}
pub fn arena_old_bytes() -> usize {
    0
}
pub fn gc_epoch() -> u64 {
    0
}
