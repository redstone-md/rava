//! Host-side classification and diagnostics for opt-in arena primitive arrays.

#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
struct ArenaPrimitiveArray {
    object: crate::arena::ArenaPtr,
    atype: u8,
    len: usize,
}

#[cfg(feature = "next-gen-gc")]
fn decode(arr: i64) -> Option<ArenaPrimitiveArray> {
    if !crate::jit::arena_primitive_arrays_enabled() {
        return None;
    }
    let address = arr as u64;
    if !crate::nextgen::is_arena_obj_addr(address) {
        return None;
    }
    // SAFETY: is_arena_obj_addr proves non-null, alignment and the current arena
    // high-water bound before either the class header or payload is observed.
    let object = unsafe { crate::arena::ArenaPtr::from_addr(address) };
    let atype = crate::nextgen::primitive_array_atype(object)?;
    let len = crate::nextgen::arena_nfields(object) as usize;
    Some(ArenaPrimitiveArray { object, atype, len })
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_primitive_array_len(arr: i64) -> Option<usize> {
    decode(arr).map(|array| array.len)
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn is_arena_primitive_array(arr: i64) -> bool {
    decode(arr).is_some()
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn verify_arena_primitive_array_load(
    arr: i64,
    idx: i64,
    native_bits: i64,
    site: i64,
) -> bool {
    let Some(array) = decode(arr) else {
        return false;
    };
    if idx < 0 || idx as usize >= array.len {
        eprintln!(
            "[LOADDIV] raw bounds atype={} len={} idx={idx} site={site}",
            array.atype, array.len
        );
        return true;
    }
    let cell = crate::nextgen::arena_cell(array.object, idx as usize);
    let observed = match array.atype {
        4 => (cell as u64 & 1) as i64,
        5 => cell as u16 as i64,
        6 => cell as u32 as i64,
        7 | 11 => cell,
        8 => cell as u8 as i8 as i64,
        9 => cell as u16 as i16 as i64,
        10 => cell as u32 as i32 as i64,
        _ => unreachable!("exact primitive-array CID decoded an invalid atype"),
    };
    if observed != native_bits {
        eprintln!(
            "[LOADDIV] raw atype={} len={} idx={idx} site={site} native={native_bits:#x} observed={observed:#x}",
            array.atype, array.len
        );
    }
    true
}
