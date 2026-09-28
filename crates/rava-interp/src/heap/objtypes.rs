//! The object, lambda and map payloads a handle can point at.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::heap::*;

impl serde::Serialize for FieldBlob {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let n = self.len();
        let mut seq = s.serialize_seq(Some(n))?;
        let (ids, cells, kinds) = (self.ids(), self.cells(), self.kinds());
        for i in 0..n {
            seq.serialize_element(&(ids[i], cells[i], kinds[i]))?;
        }
        seq.end()
    }
}

impl<'de> serde::Deserialize<'de> for FieldBlob {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let triples: Vec<(u32, i64, u8)> = serde::Deserialize::deserialize(d)?;
        let mut b = FieldBlob::new();
        b.reserve(triples.len());
        for (id, cell, kind) in triples {
            b.push(id, cell, kind);
        }
        Ok(b)
    }
}

/// Is a flat reference-array cell a boxed entry (an `idx | ABOX_TAG` index into
/// `array_box`)? A box cell is EXACTLY one whose high word is `ABOX_TAG >> 32`.
/// This must be matched precisely, not by exclusion: under next-gen-gc a ref-array
/// cell can hold a raw arena pointer (`encode_ref_cell`'s `Value::Obj` arm: an
/// 8-aligned address < 2^40, so high word < 256), which an exclusion test
/// ("non-zero, not REF, not ARRAY → box") misreads as a box and then indexes
/// `array_box[low32(addr)]` out of bounds during compaction. REF (hi=256),
/// ARRAY (hi=1024), arena pointers (hi<256) and null all fail this test.
#[inline]
pub(crate) fn is_box_cell(cell: i64) -> bool {
    (cell as u64 >> 32) as i64 == ABOX_TAG >> 32
}

/// A lambda / method-reference closure produced by `LambdaMetafactory`:
/// the (shared) implementation-method identity plus the variables captured at creation.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Lambda {
    /// MethodHandle reference kind (6=static, 5=virtual, 7=special, 9=interface).
    pub kind: u8,
    /// Interned, shared call-site identity (owner/name/descriptor).
    pub site: Arc<LambdaSite>,
    pub captured: Vec<Value>,
}

impl Lambda {
    #[inline]
    pub(crate) fn captured_bytes(&self) -> usize {
        self.captured.len() * SLOT_BYTES
    }

    #[inline]
    pub fn owner(&self) -> &str {
        &self.site.owner
    }
    #[inline]
    pub fn name(&self) -> &str {
        &self.site.name
    }
    #[inline]
    pub fn descriptor(&self) -> &str {
        &self.site.descriptor
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Object {
    /// Interned class name — millions of objects share the same few hundred
    /// class names, so an `Arc<str>` avoids a full `String` copy per object.
    pub class: Arc<str>,
    /// Instance fields as a struct-of-arrays (ids/cells/kinds), keyed by interned
    /// field-name id (see `Heap::field_id`), packed into ONE `FieldBlob` allocation.
    /// Each field is a flat `i64` cell + a 1-byte kind tag (FK_*), replacing the
    /// 24-byte `Value` enum: this both shrinks the heap AND lets compiled code read a
    /// primitive field as a native load. The kind disambiguates the type-less
    /// `get_field` (a flat `0` is `Int(0)` for kind 0 but `null` for kind 6).
    /// Reference/array fields hold the bare handle (kind 4/5); Str / sentinels go
    /// through the heap's `field_box` side table (kind 7).
    pub fields: FieldBlob,
    /// Set when this object is a functional-interface closure. BOXED so the ~40-byte
    /// Lambda costs 8 bytes (a null ptr) on the millions of NON-lambda objects.
    pub lambda: Option<Box<Lambda>>,
}

impl serde::Serialize for ObjectSlot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.get() {
            Some(object) => object.serialize(serializer),
            None => Object {
                class: Arc::from(""),
                fields: FieldBlob::new(),
                lambda: None,
            }
            .serialize(serializer),
        }
    }
}

// Field kind tags (the `i64` cell's interpretation).
impl serde::Serialize for SnapshotArrays<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;

        let arrays = self.0.arrays.read().unwrap_or_else(|e| e.into_inner());
        let array_descs = self.0.array_descs.read().unwrap_or_else(|e| e.into_inner());
        let mut sequence = serializer.serialize_seq(Some(arrays.len()))?;
        for (index, array) in arrays.iter().enumerate() {
            let element = self
                .0
                .array_element_name(array_descs.get(index).map(|d| d.element_id).unwrap_or(0))
                .map(|a| a.to_string())
                .unwrap_or_default();
            sequence.serialize_element(&SnapshotArrayRef {
                element: &element,
                data: &array.data,
            })?;
        }
        sequence.end()
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct MapObj {
    /// MUTATION COUNTER, bumped by every operation in `heap::maps` that changes what
    /// `map_entries` would return. It exists so a derived VIEW can be cached and proven
    /// current without re-deriving it: `entrySet()` materialises one `Map$Entry` handle per
    /// mapping plus three string-keyed field writes each, measured at **6.1 us on a
    /// four-entry map** (`bench/BridgeCost.java`; the JVM answers 0 because it returns a
    /// cached lazy view). Every mutation path lives in that one file — `map_put` (both the
    /// replace and the insert branch), `map_remove`, `map_move_to_first`, `map_clear` — so
    /// the counter is complete by construction, and a reader that finds it unchanged is
    /// looking at the same mapping set it derived from.
    pub(crate) version: u32,
    pub(crate) entries: Vec<(Value, Value)>,
    /// `FastMap` (FxHasher), NOT `std::HashMap` (SipHash via `RandomState`).
    ///
    /// SOUNDNESS, and it is the whole argument: this map is a LOOKUP ACCELERATOR only. A guest
    /// map's iteration order comes from `entries`, and every traversal of `index` in this file is
    /// order-independent (byte accounting at two sites, `values_mut` remaps at two more). So the
    /// hasher cannot be guest-visible — which is exactly the property that makes swapping it a
    /// one-line change rather than a divergence risk.
    ///
    /// WHY: caller-attributed PC sampling put `memchr` at 3-6% of the serial prefix and named its
    /// callers as string-keyed map lookups, with `hashbrown::HashMap::get<MapKey, usize,
    /// std::hash::random::RandomState>` on the stack — i.e. guest map lookups paying SipHash while
    /// the rest of the tree already uses `FxHasher`.
    pub(crate) index: crate::FastMap<MapKey, usize>,
    /// GAP-P04 (`RAVA_MAPKEY_SALT`): per-(class, guest hashCode) MONOTONIC next-salt
    /// counters for equals-verified salted value-object keys ("o:gS{class}:{hc}#{n}").
    /// Counters never decrease (a removed entry's salt is never reused, so a hole can
    /// never alias). Not serialized — deterministically rebuilt on restore by scanning
    /// the index's salted key strings, so the snapshot wire is unchanged.
    #[serde(skip)]
    pub(crate) salt_next: crate::FastMap<Box<str>, crate::FastMap<i32, u32>>,
    /// LRU RE-ORDERING, ANSWERED ONCE PER MAP.
    ///
    /// `Map.get` has to know whether this map is access-ordered, because a hit on an
    /// access-ordered `LinkedHashMap` moves its entry to the end. It asked with a by-NAME
    /// `$accessOrder` field probe ON EVERY HIT, and a by-name probe that MISSES is not free —
    /// which is the whole of the difference `bench/NativeCost.java` measures between `get` and
    /// `containsKey` on the identical map: 607 ns against 66. `None` = not asked yet; the field
    /// is written once, in `<init>`, so one probe per map answers it for good.
    #[serde(skip)]
    pub(crate) access_order: Option<bool>,
}

impl MapObj {
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}


/// Census: a cheap equality signature over a lambda's captured Values, for the
/// duplicate-capture estimate. Handles/interned-ptrs make it a sound equality PROXY (two lambdas
/// capturing `Ref(5)`/the same interned Str/the same arena Obj hash equal because they ARE the same
/// object); a collision only inflates the "distinct" count slightly. `len` is folded in so
/// different-arity captures never alias.
pub(crate) fn capture_sig(captured: &[crate::Value]) -> u64 {
    use crate::Value;
    let mut sig: u64 = 0x9E37_79B9_7F4A_7C15 ^ captured.len() as u64;
    for v in captured {
        let (tag, payload): (u64, u64) = match v {
            Value::Int(i) => (1, *i as u32 as u64),
            Value::Long(l) => (2, *l as u64),
            Value::Float(f) => (3, f.to_bits() as u64),
            Value::Double(d) => (4, d.to_bits()),
            Value::Str(s) => (5, s.as_ptr() as u64),
            Value::Ref(h) => (6, *h as u64),
            Value::Array(h) => (7, *h as u64),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => (8, p.addr()),
            Value::Null => (9, 0),
            Value::StdOut => (10, 0),
            Value::StdErr => (11, 0),
            #[allow(unreachable_patterns)]
            _ => (12, 0),
        };
        sig = sig.rotate_left(11) ^ (tag << 56) ^ payload.wrapping_mul(0x100_0000_01B3);
    }
    sig
}
