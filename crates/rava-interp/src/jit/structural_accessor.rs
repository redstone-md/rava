//! Immutable lazy linkage for small arena value-computer accessors.
//!
//! Entries are keyed by the complete pair `(method selector, arena class id)`.
//! A call merely triggers resolution of that pair; receiver frequency, compile
//! order never changes the structural answer. Each published word is immutable,
//! generated code verifies the whole key, and a table collision keeps the callback.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub(crate) const OP_REFUSED: u8 = 15;
pub(crate) const OP_INT_FIELD_ADD_ONE: u8 = 1;
pub(crate) const OP_INT_ARG_DIV_FIELD: u8 = 2;
/// `()I` plain own-int-field getter: `aload_0; getfield <int-family>; ireturn`.
pub(crate) const OP_INT_FIELD: u8 = 3;
/// `()I` literal body: `iconst/bipush/sipush; ireturn`. The value rides the entry's
/// slot field as `v + 1` (always ≥ 1, so the reader's `ready` test holds).
pub(crate) const OP_INT_CONST: u8 = 4;

const SLOT_BITS: u32 = 12;
pub(crate) const SLOT_MASK: u64 = (1 << SLOT_BITS) - 1;
const OP_SHIFT: u32 = SLOT_BITS;
const PAYLOAD_BITS: u32 = 16;
pub(crate) const BUCKETS: usize = 16384;
pub(crate) const WAYS: usize = 1;
pub(crate) const BUCKET_MASK: u64 = (BUCKETS - 1) as u64;

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY: AtomicU64 = AtomicU64::new(0);
static TABLE: [AtomicU64; BUCKETS * WAYS] = [EMPTY; BUCKETS * WAYS];

#[derive(Clone)]
struct Selector {
    name: Box<str>,
    descriptor: Box<str>,
}

fn selectors() -> &'static Mutex<Vec<Selector>> {
    static SELECTORS: OnceLock<Mutex<Vec<Selector>>> = OnceLock::new();
    SELECTORS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Process-local canonical id for a JVM method selector. Names participate only
/// in symbolic linkage; the bytecode recognizer decides whether an entry exists.
pub(crate) fn selector_id(name: &str, descriptor: &str) -> Option<u16> {
    let mut all = selectors().lock().ok()?;
    if let Some(i) = all
        .iter()
        .position(|s| s.name.as_ref() == name && s.descriptor.as_ref() == descriptor)
    {
        return u16::try_from(i + 1).ok();
    }
    if all.len() >= u16::MAX as usize - 1 {
        return None;
    }
    all.push(Selector {
        name: name.into(),
        descriptor: descriptor.into(),
    });
    Some(all.len() as u16)
}

#[inline]
pub(crate) fn key(class_id: u32, selector: u16) -> u64 {
    ((class_id as u64) << 16) | selector as u64
}

#[inline]
pub(crate) fn bucket(key: u64) -> usize {
    let mixed = key ^ (key >> 17) ^ (key >> 31);
    (mixed as usize) & (BUCKETS - 1)
}

#[inline]
fn unpack_key(entry: u64) -> u64 {
    entry >> PAYLOAD_BITS
}

/// Whether the complete immutable key already has either a positive or negative
/// structural answer. A collision never aliases because every way stores the key.
pub(crate) fn contains(class_id: u32, selector: u16) -> bool {
    let k = key(class_id, selector);
    let base = bucket(k) * WAYS;
    TABLE[base..base + WAYS]
        .iter()
        .any(|e| unpack_key(e.load(Ordering::Acquire)) == k)
}

/// Publish one deterministic structural answer. A full collision bucket merely
/// keeps the callback path; no existing key is replaced.
pub(crate) fn publish(class_id: u32, selector: u16, op: u8, slot: Option<u32>) {
    let slot_p1 = match slot {
        Some(s) if s < SLOT_MASK as u32 => s + 1,
        Some(_) => return,
        None => 0,
    };
    let k = key(class_id, selector);
    let packed = (k << PAYLOAD_BITS) | ((op as u64) << OP_SHIFT) | slot_p1 as u64;
    let base = bucket(k) * WAYS;
    for cell in &TABLE[base..base + WAYS] {
        let old = cell.load(Ordering::Acquire);
        if unpack_key(old) == k {
            return;
        }
        if old == 0
            && cell
                .compare_exchange(0, packed, Ordering::Release, Ordering::Acquire)
                .is_ok()
        {
            return;
        }
    }
}

#[inline]
pub(crate) fn table_base() -> i64 {
    TABLE.as_ptr() as i64
}
