//! Structural admission and mechanics for releasing unused heap-table capacity.
//!
//! This module never chooses *which* guest data to retain: handle-table length,
//! contents and ordering remain unchanged. It only releases allocator capacity
//! proven unused by `Vec::len`, after `Heap` has established an exclusive STW
//! point and the caller republishes any raw JIT bases that may relocate.

/// Ignore small gaps so alternating allocate/collect cycles do not reallocate a
/// table for insignificant savings. Admission depends only on table shape.
pub(crate) const MIN_RECLAIMED_BYTES: usize = 1 << 20;

/// Default-on after matched RSS acceptance. The explicit zero kill switch keeps
/// the former retained-capacity behavior available for diagnosis and rollback.
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_HEAP_TABLE_TRIM")
            .map(|value| value != "0")
            .unwrap_or(true)
    })
}

/// Shrink one table only when its currently unused capacity clears the structural
/// byte floor. Returns allocator capacity actually released, never guest heap
/// bytes; callers must not add this to GC's authoritative `freed` accounting.
pub(crate) fn shrink<T>(table: &mut Vec<T>) -> usize {
    let element = std::mem::size_of::<T>();
    let before = table.capacity();
    let slack_bytes = before.saturating_sub(table.len()).saturating_mul(element);
    if element == 0 || slack_bytes < MIN_RECLAIMED_BYTES {
        return 0;
    }
    table.shrink_to_fit();
    before.saturating_sub(table.capacity()).saturating_mul(element)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_preserves_contents_and_ignores_small_slack() {
        let mut small = Vec::<u64>::with_capacity(8);
        small.extend([3, 5, 8]);
        let small_capacity = small.capacity();
        assert_eq!(shrink(&mut small), 0);
        assert_eq!(small.capacity(), small_capacity);

        let extra = MIN_RECLAIMED_BYTES / std::mem::size_of::<u64>() + 1;
        let mut large = Vec::with_capacity(extra + 3);
        large.extend([3u64, 5, 8]);
        let before = large.capacity();
        assert!(shrink(&mut large) >= MIN_RECLAIMED_BYTES);
        assert!(large.capacity() < before);
        assert_eq!(large, [3, 5, 8]);
    }
}
