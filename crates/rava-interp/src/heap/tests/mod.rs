//! Heap unit tests, lifted verbatim from the inline module.

use super::*;



pub(super) fn vals(heap: &Heap, h: u32) -> Vec<i64> {
    (0..heap.array_len(h).unwrap()).map(|i| heap.array_get(h, i).unwrap().as_long().unwrap()).collect()
}

mod arrays;
mod layout;
mod parallel;
mod snapshot;
mod trim;
