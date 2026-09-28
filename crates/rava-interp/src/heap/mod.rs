//! The object heap. Instances are flat `{class, field-map}` records addressed
//! by a `u32` handle (stored in `Value::Ref`). No GC yet — objects live for the
//! run; that's fine for bootstrapping and is replaced before long sessions.









mod fieldblob;
mod arraydata;
mod censustype;
mod arrays;
mod array_ref;
pub(crate) use array_ref::*;
mod arrays_parallel;
mod census;
mod fields;
mod gc;
mod gc_arena;
mod gc_sweep;
pub(crate) mod intern;
pub(crate) use intern::Syn;
mod lambdas;
mod maps;
pub(crate) mod objects_alloc;
mod objects;
mod snapshot;
mod statics;
mod strings;
use std::collections::HashMap;
use std::sync::Arc;

use crate::value::{Value, ABOX_TAG, ARRAY_TAG, REF_TAG};

/// No-op breakpoint target: `b gc_watch_hook`, then `watchpoint set expression -s 8 -- $arg`.
#[inline(never)]
#[no_mangle]
pub extern "C" fn gc_watch_hook(addr: u64) {
    unsafe { std::ptr::read_volatile(&addr) };
}

unsafe impl Send for FieldBlob {}
unsafe impl Sync for FieldBlob {}


#[inline]
pub(crate) const fn cow_bloom_index(object: u64, field: u32) -> usize {
    let x = (object as u32 as u64) ^ (field as u64).wrapping_mul(COW_BLOOM_FID_MUL);
    let mixed = x ^ (x >> 3) ^ (x >> 17);
    (mixed as usize) & COW_BLOOM_MASK
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod obj_size_test {
    #[test]
    fn object_is_lean_without_inline_lambda() {
        // Boxing the Lambda keeps Object small (no ~100-byte inline closure on the
        // millions of non-lambda objects). Guard against a regression that re-inlines it.
        let sz = std::mem::size_of::<super::Object>();
        assert!(sz <= 128, "Object grew to {sz} bytes — inline Lambda regressed?");
    }
}

pub mod state;
pub use state::*;
mod objtypes;
pub use objtypes::*;
mod handles;
pub(crate) use handles::*;
mod jitctx;
pub(crate) use jitctx::*;
mod gens;
pub(crate) use gens::*;
mod arraytypes;
pub(crate) use arraytypes::*;
mod fieldkinds;
pub(crate) use fieldkinds::*;
pub mod workerstate;
pub use workerstate::*;
pub(crate) use workerstate::*;
