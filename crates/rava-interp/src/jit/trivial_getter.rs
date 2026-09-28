//! Retired first-receiver getter cache. `enabled()` is permanently false.
//!
//! A call site's first successful receiver must not select which class receives
//! optimization. General method dispatch remains responsible for execution.
//! The implementation below is retained temporarily for structural replacement;
//! it is not an available runtime optimization, including under environment flags.
//!
//! Historical implementation notes:
//!
//! The receiver census of a datapack run is not led by expensive operations. It is led by
//! methods whose entire body is three bytecodes:
//!
//! ```text
//! O:net/minecraft/world/phys/shapes/IndirectMerger#size          850,176
//! O:it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap#size  461,824
//! O:net/minecraft/world/phys/shapes/DiscreteCubeMerger#size      234,752
//! ```
//!
//! all of them `aload_0; getfield f; ireturn`, all of them ARENA receivers, and every one paying
//! a compiled→host crossing to read one cell.
//!
//! The first version of this was a hand-written arm for ONE of those classes, with the class id
//! and slot published from a name the engine happened to know. That does not generalise: the
//! second class has two int fields (so shape cannot pick the slot), and a third would need a
//! third arm. **What generalises is the SITE.** A call site's receiver class is stable in
//! practice, so one table entry per site — `(class id, slot)` — serves whatever that site sees,
//! and the class-id compare in the emitted guard is what makes it correct rather than lucky:
//! a receiver of any other class takes exactly the callback it took before.
//!
//! ## The entry is written by the call it removes
//!
//! `jit_call_body` sees the concrete receiver AND resolves the callee. When a zero-argument
//! virtual/interface call on an arena receiver resolves to a body of exactly
//! `aload_0; getfield #x; <t>return`, the field's slot for that class is recorded here. The next
//! compile of that site bakes the entry's ADDRESS, and the emitted code reads the pair at
//! runtime — so an entry recorded after the method compiled still takes effect, which is the
//! bootstrap trap that kept three earlier arms at zero for months.
//!
//! ## What it deliberately does not do
//!
//! * **No name matching.** The eligibility test is the callee's BYTECODE, which is permitted
//!   evidence; the class name never enters it.
//! ## ⛔And it is NOT emitted anywhere else, which cost a 30% regression to learn
//!
//! The obvious generalisation — emit this guard at EVERY zero-argument primitive-returning
//! call site, not only on the arena edge of an arm that already exists — was built, measured
//! and reverted: **CPU 31.25 s against 21.78 s disjoint, 30% SLOWER.** It served 552,152 calls
//! inline and still lost, because most of those sites were not paying a host crossing at all:
//! an ordinary guest getter on a compiled class goes through the machine-code PIC straight into
//! compiled code, and replacing that with a guard plus a `fu_invoke` callback on the miss is a
//! large downgrade. Sites placed after `fu_routable` (`Enum.ordinal`, the BitSet helpers) were
//! stolen from their own faster helpers as well.
//!
//! ⭐**An inline arm is only a win where the call it replaces was a CROSSING.** Counting the
//! calls it can answer says nothing about that, and this table's population was counted from
//! `fu_fastpath`'s census — i.e. from crossings — which is why the chained use pays and the
//! general one does not.
//!
//! * **No handle receivers.** A handle object's fields are not at a fixed offset, so the same
//!   trick needs a different read and gets a different arm.
//! * **No overwrite.** An entry is written once. A megamorphic site therefore keeps whichever
//!   class it met first and the others take the callback — a wrong guess costs a compare, not
//!   correctness.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Direct-mapped, power of two. Sized like `FU_SITES`: a boot compiles a few thousand sites and
/// only the zero-argument arena ones ever land here.
pub(crate) const SITES: usize = 8192;

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY: AtomicU64 = AtomicU64::new(0);
/// `(class id + 1) << 32 | (slot + 1)`, or 0 for "nothing known about this site".
pub(crate) static TABLE: [AtomicU64; SITES] = [EMPTY; SITES];
/// ⚠**THE KEY IS NOT OPTIONAL, AND OMITTING IT CRASHED THE RUN.** The table is direct-mapped,
/// so two sites can hash to one row. The first version stored only the value; while the arm was
/// emitted at a few hundred sites nothing collided, and widening it to every zero-argument call
/// site produced an access violation on the first boot — one site reading another site's slot,
/// on a receiver whose class-id compare had already "passed" because it was checking the
/// colliding entry's id. A row now carries the site identity and is only believed when it
/// matches, which turns a collision into a callback instead of a wrong load.
pub(crate) static KEYS: [AtomicU64; SITES] = [EMPTY; SITES];

/// The site's identity, hashed the same way `fu_site_slot` hashes it — a caller class pointer
/// plus a constant-pool index.
#[inline]
pub(crate) fn slot_for(caller: usize, cp: u16) -> usize {
    let k = (caller as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (cp as u64).wrapping_mul(0x517C_C1B7);
    ((k >> 17) as usize) & (SITES - 1)
}

/// The site's identity, stored beside the value and verified before the value is used.
#[inline]
pub(crate) fn key_for(caller: usize, cp: u16) -> u64 {
    (caller as u64) ^ ((cp as u64) << 48)
}

/// The addresses the JIT bakes: the key cell and the value cell. Both are read at RUNTIME, so a
/// site compiled before its receiver class existed still picks the entry up.
#[inline]
pub(crate) fn entry_addr(caller: usize, cp: u16) -> (i64, i64) {
    let i = slot_for(caller, cp);
    (
        (&KEYS[i] as *const AtomicU64) as i64,
        (&TABLE[i] as *const AtomicU64) as i64,
    )
}

#[inline]
pub(crate) fn is_empty(caller: usize, cp: u16) -> bool {
    let i = slot_for(caller, cp);
    KEYS[i].load(Relaxed) != key_for(caller, cp) || TABLE[i].load(Relaxed) == 0
}

/// Marks the low word as a literal answer rather than a slot index. A slot is stored as
/// `slot + 1` so the word is never 0 for a described site; a constant sets bit 31, which no
/// real slot index can reach.
pub(crate) const CONST_BIT: u64 = 0x8000_0000;

/// Record a constant answer for a site.
pub(crate) fn record_const(caller: usize, cp: u16, cid: u32, value: i32) {
    if value < 0 {
        return;
    }
    write(caller, cp, ((cid as u64 + 1) << 32) | CONST_BIT | value as u64);
}

/// Record `(class id, slot)` for a site. First writer wins — see the module note on megamorphic
/// sites.
pub(crate) fn record(caller: usize, cp: u16, cid: u32, slot: usize) {
    if slot as u64 + 1 >= CONST_BIT {
        return;
    }
    write(caller, cp, ((cid as u64 + 1) << 32) | (slot as u64 + 1))
}

fn write(caller: usize, cp: u16, packed: u64) {
    let i = slot_for(caller, cp);
    // Value first, then key: a reader that sees the key sees a value already published, and a
    // reader that arrives between the two sees a stale key and takes the callback.
    if TABLE[i].compare_exchange(0, packed, Relaxed, Relaxed).is_ok() {
        KEYS[i].store(key_for(caller, cp), Relaxed);
        RECORDED.fetch_add(1, Relaxed);
    }
}

/// Entries written — how many sites the recorder ever managed to describe.
pub(crate) static RECORDED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn recorded() -> u64 {
    RECORDED.load(Relaxed)
}

/// Retired: first-writer receiver selection violates no-adapt admission.
/// The constant lets compilation omit both recording and emitted lookup arms.
pub(crate) const fn enabled() -> bool {
    false
}

/// A body that returns a small non-negative CONSTANT: `iconst_<n>`, `bipush b` or `sipush s`
/// followed by `ireturn`, and nothing else.
///
/// ⭐**The second-largest refusal family is this shape, not a field read.** DataFixerUpper's
/// type templates answer `size()` with a literal — `Const` 87,001, `Sum` 46,825, `Tag` 44,690,
/// `RecursivePoint` 26,912, `Product` 23,301, `Named` 14,101, `List` 12,254 and three more,
/// about 259,000 crossings a datapack run to push an integer that is written in the bytecode.
/// A class-id compare is still what makes it correct: the constant belongs to the class the
/// entry was recorded for.
pub(crate) fn trivial_const(code: &[u8]) -> Option<i32> {
    match code {
        [op @ 0x03..=0x08, 0xac] => Some(*op as i32 - 0x03),
        [0x10, b, 0xac] => Some(*b as i8 as i32),
        [0x11, hi, lo, 0xac] => Some((((*hi as u16) << 8) | *lo as u16) as i16 as i32),
        _ => None,
    }
    .filter(|v| *v >= 0)
}

/// The whole eligibility test: `aload_0; getfield #x; <t>return`, and nothing else.
///
/// Returns the constant-pool index of the field. A body with a wide index, a null check, or a
/// second instruction is simply not eligible — this is a recogniser, not a compiler.
pub(crate) fn trivial_getfield(code: &[u8]) -> Option<u16> {
    // 0x2a aload_0 | 0xb4 getfield idx_hi idx_lo | one of the returns
    if code.len() != 5 || code[0] != 0x2a || code[1] != 0xb4 {
        return None;
    }
    match code[4] {
        // ireturn, lreturn, freturn, dreturn, areturn
        0xac | 0xad | 0xae | 0xaf | 0xb0 => {}
        _ => return None,
    }
    Some(((code[2] as u16) << 8) | code[3] as u16)
}
