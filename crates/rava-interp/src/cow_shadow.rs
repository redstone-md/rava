//! Authoritative per-thread COW field shadow.
//!
//! The map owns the bookkeeping for its active JIT context's counting Bloom
//! filter. Keeping both lifecycles in one type prevents a
//! clear, panic, or TLS destructor from leaving an unsafe false-negative state.

use crate::Value;
use std::collections::HashMap;

pub(crate) struct ShadowFields {
    fields: HashMap<(u64, u32), Value>,
}

impl ShadowFields {
    pub(crate) fn new() -> Self {
        Self {
            fields: HashMap::new(),
        }
    }

    #[inline]
    pub(crate) fn insert(&mut self, object: u64, field: u32, value: Value) {
        if self.fields.insert((object, field), value).is_none() {
            crate::heap::cow_bloom_note(object, field);
        }
    }

    #[inline]
    pub(crate) fn get_cloned(&self, object: u64, field: u32) -> Option<Value> {
        self.fields.get(&(object, field)).cloned()
    }

    pub(crate) fn clear(&mut self) {
        for &(object, field) in self.fields.keys() {
            crate::heap::cow_bloom_remove(object, field);
        }
        self.fields.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.fields.len()
    }
}

impl Drop for ShadowFields {
    fn drop(&mut self) {
        self.clear();
    }
}
