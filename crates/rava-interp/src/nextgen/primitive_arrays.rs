//! Process-stable arena class identities for JVM primitive arrays.
//!
//! The class id is the representation tag used by compiled array operations. It is
//! established once, before compilation starts, and decoded through this exact table;
//! synthetic registry names are diagnostic labels only and never make a routing decision.

use crate::arena::ArenaPtr;
use std::sync::{Arc, OnceLock};

const FIRST_ATYPE: u8 = 4;
const LAST_ATYPE: u8 = 11;
const ATYPE_COUNT: usize = (LAST_ATYPE - FIRST_ATYPE + 1) as usize;

/// Arena primitive-array elements occupy the GC's ordinary eight-byte cells.
pub const PRIMITIVE_ARRAY_CELL_BYTES: usize = std::mem::size_of::<i64>();

const LABELS: [&str; ATYPE_COUNT] = [
    "[arena-primitive-array:atype-4]",
    "[arena-primitive-array:atype-5]",
    "[arena-primitive-array:atype-6]",
    "[arena-primitive-array:atype-7]",
    "[arena-primitive-array:atype-8]",
    "[arena-primitive-array:atype-9]",
    "[arena-primitive-array:atype-10]",
    "[arena-primitive-array:atype-11]",
];

fn primitive_array_cids() -> &'static [u32; ATYPE_COUNT] {
    static CIDS: OnceLock<[u32; ATYPE_COUNT]> = OnceLock::new();
    CIDS.get_or_init(|| {
        let empty: Arc<[(u32, u8)]> = Arc::from(Vec::<(u32, u8)>::new());
        std::array::from_fn(|i| {
            // Names only deduplicate and label registry entries. All consumers route by
            // the exact class ids retained in CIDS, never by inspecting these strings.
            let label: Arc<str> = Arc::from(LABELS[i]);
            super::register_class(&label, &empty)
        })
    })
}

/// Materialize all eight class ids in JVM `newarray` atype order (4 through 11).
///
/// `Vm::new` calls this before constructing the background compiler, making the
/// representation identities immutable before either compiler can observe them.
pub fn init_primitive_array_classes() {
    let _ = primitive_array_cids();
}

/// Return the exact arena class id for a valid JVM `newarray` atype.
#[inline]
pub fn primitive_array_cid(atype: u8) -> Option<u32> {
    let index = atype.checked_sub(FIRST_ATYPE)? as usize;
    (index < ATYPE_COUNT).then(|| primitive_array_cids()[index])
}

/// Decode a class id only when it is one of the eight registered primitive-array ids.
#[inline]
pub fn primitive_array_atype_for_cid(cid: u32) -> Option<u8> {
    primitive_array_cids()
        .iter()
        .position(|&registered| registered == cid)
        .map(|i| FIRST_ATYPE + i as u8)
}

/// Decode the JVM atype from an exact arena object pointer's class header.
#[inline]
pub fn primitive_array_atype(p: ArenaPtr) -> Option<u8> {
    primitive_array_atype_for_cid(super::arena_class_id(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_object(class_id: u32) -> (super::super::Region, ArenaPtr) {
        let region = super::super::Region::new(super::super::obj_size(1));
        let raw = region.base();
        unsafe { super::super::obj_init_new(raw, class_id, 1) };
        (region, ArenaPtr::new(raw).expect("mapped arena object"))
    }

    #[test]
    fn primitive_array_ids_are_distinct_and_stable() {
        init_primitive_array_classes();
        let first: [u32; ATYPE_COUNT] = std::array::from_fn(|i| {
            primitive_array_cid(FIRST_ATYPE + i as u8).expect("valid primitive atype")
        });
        let second: [u32; ATYPE_COUNT] = std::array::from_fn(|i| {
            primitive_array_cid(FIRST_ATYPE + i as u8).expect("stable primitive atype")
        });

        assert_eq!(first, second);
        for (i, &cid) in first.iter().enumerate() {
            assert_eq!(
                primitive_array_atype_for_cid(cid),
                Some(FIRST_ATYPE + i as u8)
            );
            assert!(
                !first[..i].contains(&cid),
                "each atype needs a distinct class id"
            );
            assert!(super::super::mask_empty(&super::super::ref_mask_of(cid)));
            assert!(!super::super::class_all_refs(cid));
            assert!(
                super::super::class_layout(cid)
                    .expect("registered class")
                    .1
                    .is_empty()
            );
        }
        assert_eq!(PRIMITIVE_ARRAY_CELL_BYTES, 8);
    }

    #[test]
    fn primitive_array_decode_rejects_every_non_registry_identity() {
        init_primitive_array_classes();
        assert_eq!(primitive_array_cid(FIRST_ATYPE - 1), None);
        assert_eq!(primitive_array_cid(LAST_ATYPE + 1), None);
        assert_eq!(primitive_array_atype_for_cid(u32::MAX), None);

        // The older shared primitive-array class may sit adjacent in the registry, but
        // adjacency is not identity: decoding must consult the exact eight-entry table.
        let generic_primitive_cid = super::super::arena_array_class(false);
        assert_eq!(primitive_array_atype_for_cid(generic_primitive_cid), None);
        let (_region, generic) = isolated_object(generic_primitive_cid);
        assert_eq!(primitive_array_atype(generic), None);
    }

    #[test]
    fn primitive_array_pointer_decodes_its_exact_header_class() {
        let cid = primitive_array_cid(10).expect("int[] class id");
        let (_region, p) = isolated_object(cid);
        assert_eq!(primitive_array_atype(p), Some(10));
    }
}
