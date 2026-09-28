//! The array payload and its element typing.

use crate::heap::*;

/// ⭐**RAVA_NO_FAST_ARRAYDATA=1 restores the pre-round array-storage paths verbatim.**
///
/// Everything behind this gate is *plumbing around* array storage, never its meaning: the fast
/// arms hand back the same bytes, the same descriptor and the same handle span as the legacy
/// arms. What they remove is repeated derivation — one discriminant switch per ELEMENT where the
/// storage kind is a constant of the ARRAY, and one spine reallocation per parallel REGION where
/// the spine only ever grows by the arrays a region actually keeps.
///
/// A relaxed `AtomicU8`, not a `OnceLock`: the same argument `FAST_ARRAYS` makes next door — an
/// acquire load LLVM may refuse to hoist is not free on a path reached per bulk copy. On x86 this
/// is a `mov` from a permanently L1-resident static and a perfectly predicted branch.
/// **Table size: 1 byte.**
static FAST_ARRAYDATA: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

#[inline(always)]
pub(crate) fn fast_arraydata() -> bool {
    match FAST_ARRAYDATA.load(std::sync::atomic::Ordering::Relaxed) {
        1 => true,
        0 => false,
        _ => fast_arraydata_init(),
    }
}

/// Read the environment once, off the hot path. `emit_more_disabled()` is consulted because this
/// belongs to the same "emit more, cross less" block and has to fall with it.
#[cold]
#[inline(never)]
fn fast_arraydata_init() -> bool {
    let on = crate::flags::round7()
        && std::env::var_os("RAVA_NO_FAST_ARRAYDATA").is_none()
        && !crate::flags::emit_more_disabled();
    FAST_ARRAYDATA.store(on as u8, std::sync::atomic::Ordering::Relaxed);
    on
}

impl ArrayData {
    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self {
            ArrayData::Byte(v) => v.len(),
            ArrayData::Int(v) => v.len(),
            ArrayData::Long(v) => v.len(),
            ArrayData::Float(v) => v.len(),
            ArrayData::Double(v) => v.len(),
            ArrayData::Ref(v) => v.len(),
        }
    }
    /// Primitive element read. Reference arrays return `None` — the heap decodes
    /// those cells via `array_box` in `Heap::array_get`.
    #[inline]
    pub(crate) fn get(&self, i: usize) -> Option<Value> {
        Some(match self {
            ArrayData::Byte(v) => Value::Int(*v.get(i)? as i8 as i32),
            ArrayData::Int(v) => Value::Int(*v.get(i)?),
            ArrayData::Long(v) => Value::Long(*v.get(i)?),
            ArrayData::Float(v) => Value::Float(*v.get(i)?),
            ArrayData::Double(v) => Value::Double(*v.get(i)?),
            ArrayData::Ref(_) => return None,
        })
    }
    #[inline]
    pub(crate) fn set(&mut self, i: usize, val: Value) {
        match self {
            ArrayData::Byte(v) => { if let Some(s) = v.get_mut(i) { *s = val.as_int().unwrap_or(0) as u8; } }
            ArrayData::Int(v) => { if let Some(s) = v.get_mut(i) { *s = val.as_int().unwrap_or(0); } }
            ArrayData::Long(v) => { if let Some(s) = v.get_mut(i) { *s = val.as_long().unwrap_or(0); } }
            ArrayData::Float(v) => { if let Some(s) = v.get_mut(i) { *s = val.as_f64() as f32; } }
            ArrayData::Double(v) => { if let Some(s) = v.get_mut(i) { *s = val.as_f64(); } }
            ArrayData::Ref(_) => {} // heap encodes ref cells via array_box
        }
    }
    pub(crate) fn to_values(&self) -> Vec<Value> {
        // One gate read PER ARRAY (not per element) — RAVA_NO_FAST_ARRAYS restores the old
        // per-element re-dispatch verbatim for an A/B.
        if !fast_arrays() {
            return (0..self.len()).map(|i| self.get(i).unwrap_or(Value::Null)).collect();
        }
        let mut out = Vec::new();
        self.values_into(&mut out);
        out
    }

    /// Append every element to `out`.
    ///
    /// ⭐**ONE DISCRIMINANT SWITCH FOR THE WHOLE ARRAY, NOT ONE PER ELEMENT.** The previous
    /// `to_values` was `(0..len).map(|i| self.get(i))`: per element that is a switch over the six
    /// storage kinds, a `Vec::len` load, a bounds check, an `Option` construction and an
    /// `unwrap_or` — five pieces of plumbing around a `mov`. This engine reaches here on every
    /// drain of every `$elems`-backed collection (`Heap::array_values`), i.e. on every
    /// `iterator`, `toArray`, `stream`, `toString` and copy-constructor, so the per-element
    /// dispatch was being paid millions of times a boot to answer a question that is constant
    /// for the array. Hoisting it also lets `collect`/`extend` see a real `ExactSizeIterator`
    /// and reserve once instead of growing geometrically.
    ///
    /// Reference arrays are NOT handled here — their cells decode through `Heap::array_box`,
    /// which this type cannot reach; `Heap::array_values` owns that arm.
    pub(crate) fn values_into(&self, out: &mut Vec<Value>) {
        out.reserve(self.len());
        match self {
            // A `byte[]` element is SIGNED in the JVM and a `boolean[]` is 0/1, for which the
            // sign extension is a no-op — one arm serves both, exactly as `get` did.
            ArrayData::Byte(v) => out.extend(v.iter().map(|&x| Value::Int(x as i8 as i32))),
            ArrayData::Int(v) => out.extend(v.iter().map(|&x| Value::Int(x))),
            ArrayData::Long(v) => out.extend(v.iter().map(|&x| Value::Long(x))),
            ArrayData::Float(v) => out.extend(v.iter().map(|&x| Value::Float(x))),
            ArrayData::Double(v) => out.extend(v.iter().map(|&x| Value::Double(x))),
            // Matches the old behaviour bit for bit: `get` answered `None` for a ref array and
            // the caller's `unwrap_or(Value::Null)` turned every cell into null.
            ArrayData::Ref(v) => out.extend((0..v.len()).map(|_| Value::Null)),
        }
    }
    /// Base pointer of the flat element buffer, or null for reference arrays
    /// (whose `Value` backing is not a native primitive buffer). Stable for the
    /// array's lifetime: Java arrays are fixed-length so the Vec never reallocs.
    #[inline]
    pub(crate) fn data_ptr(&self) -> *const u8 {
        match self {
            ArrayData::Byte(v) => v.as_ptr(),
            ArrayData::Int(v) => v.as_ptr() as *const u8,
            ArrayData::Long(v) => v.as_ptr() as *const u8,
            ArrayData::Float(v) => v.as_ptr() as *const u8,
            ArrayData::Double(v) => v.as_ptr() as *const u8,
            ArrayData::Ref(v) => v.as_ptr() as *const u8, // flat tagged ref cells
        }
    }
    /// Base pointer AND element count out of ONE discriminant switch.
    ///
    /// WAS: `data_ptr()` immediately followed by `len()` — two jump tables over the six storage
    /// kinds to read two adjacent fields of the SAME `Vec` header. Paid once per array published
    /// into a parallel region (`worker_array_put`), i.e. once per array a chunk task allocates,
    /// which is the population `ChunkFill` scales. IS: one switch, one header, both fields.
    /// Byte-identical to calling the two accessors in sequence.
    #[inline]
    pub(crate) fn ptr_len(&self) -> (*const u8, usize) {
        match self {
            ArrayData::Byte(v) => (v.as_ptr(), v.len()),
            ArrayData::Int(v) => (v.as_ptr() as *const u8, v.len()),
            ArrayData::Long(v) => (v.as_ptr() as *const u8, v.len()),
            ArrayData::Float(v) => (v.as_ptr() as *const u8, v.len()),
            ArrayData::Double(v) => (v.as_ptr() as *const u8, v.len()),
            ArrayData::Ref(v) => (v.as_ptr() as *const u8, v.len()),
        }
    }

    /// Lower the WHOLE array to raw `u8` with ONE discriminant switch instead of one per element.
    ///
    /// WAS (`Heap::array_bytes`'s general arm): `(0..len).map(|i| self.get(i).and_then(Value::as_int)
    /// .unwrap_or(0) as u8)`. Per BYTE that is a switch over the six storage kinds, a `Vec::len`
    /// load, a bounds check, an `Option` construction, a `Value` construction, a SECOND switch
    /// inside `as_int` and an `unwrap_or` — seven pieces of plumbing to move one byte, on the
    /// whole-array IO entry point (NBT save, resource reads, digests, channel writes). The
    /// dispatch therefore scaled with every byte the world save moves. IS: one switch, then a
    /// `memcpy`, a tight truncating loop, or a `memset`.
    ///
    /// ⚠The zero arms are NOT a shortcut and NOT a bug fix. `Value::as_int` matches `Value::Int`
    /// alone, so a `long[]`/`float[]`/`double[]`/reference array ALREADY lowered to all-zero bytes
    /// through this entry point; reproducing that exactly is what keeps the change invisible.
    /// `char[]`/`short[]`/`int[]` share the `Int` store and truncate, as they did.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        match self {
            ArrayData::Byte(v) => v.clone(),
            ArrayData::Int(v) => v.iter().map(|&x| x as u8).collect(),
            ArrayData::Long(v) => vec![0u8; v.len()],
            ArrayData::Float(v) => vec![0u8; v.len()],
            ArrayData::Double(v) => vec![0u8; v.len()],
            ArrayData::Ref(v) => vec![0u8; v.len()],
        }
    }

    /// Census: index into `HeapCensus::arr_payload` / `ARR_KIND_NAMES`.
    pub(crate) fn kind_idx(&self) -> usize {
        match self {
            ArrayData::Byte(_) => 0,
            ArrayData::Int(_) => 1,
            ArrayData::Long(_) => 2,
            ArrayData::Float(_) => 3,
            ArrayData::Double(_) => 4,
            ArrayData::Ref(_) => 5,
        }
    }

    /// Census: the REAL backing allocation, from `Vec::capacity()` (not `len`). Java arrays
    /// are fixed-length so cap == len in practice, but reading capacity keeps this
    /// allocator-true rather than assuming that.
    pub(crate) fn alloc_bytes(&self) -> usize {
        let (cap, esz) = match self {
            ArrayData::Byte(v) => (v.capacity(), 1),
            ArrayData::Int(v) => (v.capacity(), 4),
            ArrayData::Long(v) => (v.capacity(), 8),
            ArrayData::Float(v) => (v.capacity(), 4),
            ArrayData::Double(v) => (v.capacity(), 8),
            ArrayData::Ref(v) => (v.capacity(), 8),
        };
        if cap == 0 { 0 } else { cap * esz + ALLOC_TAX }
    }

    /// Estimated bytes per element, for the heap byte accounting.
    #[inline]
    pub(crate) fn elem_bytes(&self) -> usize {
        match self {
            ArrayData::Byte(_) => 1,
            ArrayData::Int(_) | ArrayData::Float(_) => 4,
            ArrayData::Long(_) | ArrayData::Double(_) => 8,
            ArrayData::Ref(_) => 8, // flat i64 cell (Str payloads live in array_box)
        }
    }

    /// Backing Vec capacity in ELEMENTS (census: to split used vs unused-capacity bytes).
    pub(crate) fn capacity(&self) -> usize {
        match self {
            ArrayData::Byte(v) => v.capacity(),
            ArrayData::Int(v) => v.capacity(),
            ArrayData::Long(v) => v.capacity(),
            ArrayData::Float(v) => v.capacity(),
            ArrayData::Double(v) => v.capacity(),
            ArrayData::Ref(v) => v.capacity(),
        }
    }

    /// True for a reference/object array (`ArrayData::Ref`) — the census Ref-vs-Prim split.
    pub(crate) fn is_ref(&self) -> bool {
        matches!(self, ArrayData::Ref(_))
    }
}
