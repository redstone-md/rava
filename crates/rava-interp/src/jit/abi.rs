//! Cell ABI: how a guest value is packed into the i64 cells compiled code exchanges with the host.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::Value as JVal;
use crate::value::{ARRAY_TAG, BOX_TAG, REF_TAG};
use super::{JT, BOX_TAG_HI};

pub(crate) const REF_TAG_HI: u64 = (REF_TAG as u64) >> 32;
pub(crate) const ARRAY_TAG_HI: u64 = (ARRAY_TAG as u64) >> 32;

/// Untag a reference cell to a heap handle. `None` = null (cell 0).
pub(crate) fn untag_ref(cell: i64) -> Option<u32> {
    let hi = (cell as u64) >> 32;
    if cell == 0 || (hi != REF_TAG_HI && hi != ARRAY_TAG_HI) {
        None
    } else {
        Some((cell & 0xFFFF_FFFF) as u32)
    }
}

pub(crate) fn untag_array(cell: i64) -> Option<u32> {
    let hi = (cell as u64) >> 32;
    if cell != 0 && hi == ARRAY_TAG_HI {
        Some((cell & 0xFFFF_FFFF) as u32)
    } else {
        None
    }
}

/// Unified low-2-bit cell ABI — MARATHON FOUNDATION (Task 1).
///
/// The endgame collapses the JIT operand-cell scheme (high-word REF_TAG/
/// ARRAY_TAG/BOX_TAG, this file) and the arena inline-cell scheme (low-2-bit, see
/// `opcodes::encode_arena_cell`) into ONE ABI, so a raw `ArenaPtr` flows through
/// compiled code as its bare pointer (low 2 bits `0b00`) with no boxing. These are
/// the unified primitives. They are INFRA in this tracer: the live hot paths still
/// run the high-word scheme (kept default for every op except the dual-path
/// getfield), so they are not wired in yet — only the dual-path getfield and the
/// `Value::Obj` raw-pointer arrival are. Tags mirror the arena cell exactly:
///   `0b00` ArenaPtr (raw, 8-aligned) / null(0)   `0b01` heap handle (Ref)
///   `0b10` array handle (Array)                  `0b11` boxed Str/sentinel
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)] // infra: wired into hot paths in Task 1, not the tracer
pub(crate) mod jit_abi {
    use crate::value::Value as JVal;

    /// Pack a `Value` into a unified low-2-bit REFERENCE-slot cell (primitives stay
    /// flat and must not use this). An `Obj` passes through as its raw pointer.
    pub(crate) fn unify_pack(v: &JVal) -> i64 {
        match v {
            // B5 PRODUCTION FLIP (RAVA_PINNED_PTRS): a mirrored handle stores as
            // its raw pinned address — the unified pointer form (see
            // opcodes::encode_arena_cell for the rationale and bridges).
            JVal::Ref(h) => {
                if crate::nextgen::pinned_ptrs_enabled() {
                    let m = crate::nextgen::pinned_mirror_get(*h);
                    if m != 0 {
                        return m as i64;
                    }
                }
                ((*h as i64) << 2) | 0b01
            }
            JVal::Array(h) => ((*h as i64) << 2) | 0b10,
            JVal::Obj(p) => p.addr() as i64, // 8-aligned → low 2 bits 0b00
            JVal::Null => 0,
            // Str / StdOut / StdErr / boxed primitive → arena side table (0b11).
            _ => ((crate::nextgen::arena_box_push(v.clone()) as i64) << 2) | 0b11,
        }
    }

    /// Split a unified cell into `(tag, payload)`: `tag` = low 2 bits; `payload` is
    /// the handle / box index (`cell >> 2`) for a tagged form, or the raw pointer
    /// bits (`cell`) for an `ArenaPtr` (tag `0b00`).
    pub(crate) fn unify_untag(cell: i64) -> (u8, i64) {
        let tag = (cell & 0b11) as u8;
        let payload = if tag == 0 { cell } else { cell >> 2 };
        (tag, payload)
    }

    /// A unified cell as a GC root. Handles (`0b01`/`0b10`) and a non-null
    /// `ArenaPtr` (`0b00`) are roots; null (0) and a boxed-Str cell (`0b11`, which
    /// owns its own `Arc`, not a handle) are not.
    pub(crate) fn unify_cell_to_root(cell: i64) -> Option<JVal> {
        match cell & 0b11 {
            0b01 => Some(JVal::Ref((cell >> 2) as u32)),
            0b10 => Some(JVal::Array((cell >> 2) as u32)),
            0b00 if cell != 0 => {
                Some(JVal::Obj(unsafe { crate::arena::ArenaPtr::from_addr(cell as u64) }))
            }
            _ => None,
        }
    }
}

/// Unpack one call argument cell to a `Value`, given its descriptor kind char
/// (`L`/`[` = reference, untagged).
pub(crate) fn unpack_arg(cell: i64, kind: u8) -> JVal {
    match kind {
        b'J' => JVal::Long(cell),
        b'F' => JVal::Float(f32::from_bits(cell as u32)),
        b'D' => JVal::Double(f64::from_bits(cell as u64)),
        b'L' | b'[' => match untag_ref(cell) {
            Some(h) => JVal::Ref(h),
            None => JVal::Null,
        },
        _ => JVal::Int(cell as i32), // I/Z/B/C/S
    }
}

/// A JIT cell as a GC root, if it holds a tagged non-null object/array handle.
/// The GC scans live JIT locals/stack cells through this (Stage 3 calls can run
/// the collector while a compiled frame is suspended on the Rust stack).
pub(crate) fn cell_to_root(cell: i64) -> Option<JVal> {
    // Raw `ArenaPtr` arrival (next-gen-gc): a `Value::Obj` packs as its bare
    // 8-aligned pointer (< 2^40, no high-word tag), so it must be reported as a
    // root or the moving GC frees a live arena receiver held only in a compiled
    // local. `cell >> 40 == 0` is disjoint from the high-word handle tags below
    // (`hi >= 256`). Checked first so the high-word match stays byte-identical.
    #[cfg(feature = "next-gen-gc")]
    if cell != 0 && (cell as u64) >> 40 == 0 {
        return Some(JVal::Obj(unsafe { crate::arena::ArenaPtr::from_addr(cell as u64) }));
    }
    let h = (cell & 0xFFFF_FFFF) as u32;
    match (cell as u64) >> 32 {
        REF_TAG_HI => Some(JVal::Ref(h)),
        ARRAY_TAG_HI => Some(JVal::Array(h)),
        _ => None,
    }
}

/// Pack an interpreter local `Value` into the i64 cell the compiled code reads.
/// Floats occupy the low 32 bits (zero-extended); heap handles carry a kind tag.
pub fn pack_local(v: &JVal) -> i64 {
    match v {
        JVal::Int(i) => *i as i64,
        JVal::Long(l) => *l,
        JVal::Float(f) => f.to_bits() as u32 as i64,
        JVal::Double(d) => d.to_bits() as i64,
        // B6a OPERAND FLIP (RAVA_PINNED_PTRS): a mirrored handle enters compiled
        // code as its raw pinned ADDRESS — the receiver then takes the inline
        // ARENA arm of every field op (no handle callback), which is the arc's
        // payoff path. Boundary materialization (unbox_ref/decode) maps it back
        // to Ref(h) on the Rust side.
        #[cfg(feature = "next-gen-gc")]
        JVal::Ref(h) => {
            if crate::nextgen::pinned_ptrs_enabled() {
                let m = crate::nextgen::pinned_mirror_get(*h);
                if m != 0 {
                    return m as i64;
                }
            }
            (*h as i64) | REF_TAG
        }
        #[cfg(not(feature = "next-gen-gc"))]
        JVal::Ref(h) => (*h as i64) | REF_TAG,
        JVal::Array(h) => (*h as i64) | ARRAY_TAG,
        // Arena receiver (next-gen-gc): the raw 8-aligned pointer IS the cell. It
        // is < 2^40 (low-address arena), so it never collides with a high-word tag
        // and compiled code reads its inline fields directly off the pointer.
        #[cfg(feature = "next-gen-gc")]
        JVal::Obj(p) => p.addr() as i64,
        _ => 0, // Null and unsupported -> 0
    }
}

/// True when a reference cell is a `BOX_TAG` index into the transient
/// `jit_box_table` (as opposed to a real handle / null) — such a cell must be
/// re-homed before the table unwinds (see `Vm::jit_direct_static`).
pub(crate) fn is_box_cell(cell: i64) -> bool {
    (cell as u64 >> 32) == BOX_TAG_HI && cell != 0
}

/// Pack a `Value` headed for a REFERENCE slot. Heap handles keep the flat
/// tagged-handle form (the getfield/aload callbacks and the GC untag them
/// directly, preserving object-vs-array kind); null is 0; everything else — a
/// `Str`, a `StdOut`/`StdErr` sentinel, or a boxed primitive — is stashed in the
/// VM side-table as `index | BOX_TAG`, preserving its exact Value (identity AND
/// a zero payload). The table unwinds with the compiled frame (see `jit_run`).
/// PRIMITIVE slots must NOT use this — they pack flat via `pack_local` so
/// compiled arithmetic reads raw bits.
pub(crate) fn box_ref(_vm: &crate::Vm, v: &JVal) -> i64 {
    match v {
        // B6a OPERAND FLIP (RAVA_PINNED_PTRS): see pack_local — a mirrored handle
        // flows as its raw pinned address (inline arena arm, no handle callback).
        #[cfg(feature = "next-gen-gc")]
        JVal::Ref(h) => {
            if crate::nextgen::pinned_ptrs_enabled() {
                let m = crate::nextgen::pinned_mirror_get(*h);
                if m != 0 {
                    return m as i64;
                }
            }
            (*h as i64) | REF_TAG
        }
        #[cfg(not(feature = "next-gen-gc"))]
        JVal::Ref(h) => (*h as i64) | REF_TAG,
        JVal::Array(h) => (*h as i64) | ARRAY_TAG,
        JVal::Null => 0,
        // Arena receiver (next-gen-gc): pass the raw pointer through unboxed (it is
        // < 2^40, disjoint from every high-word tag) instead of stashing it in the
        // BOX_TAG side table — that boxing forced every arena field op cold.
        #[cfg(feature = "next-gen-gc")]
        JVal::Obj(p) => p.addr() as i64,
        _ => {
            let idx = crate::frames::box_push(v.clone());
            (idx as i64) | BOX_TAG
        }
    }
}

/// Decode a REFERENCE-slot cell back to a `Value`. Purely tag-driven: every such
/// cell was produced by `box_ref` (params, getfield/aload results, ref returns)
/// or is `aconst_null` (0), so it is exactly null / a `BOX_TAG` table index / a
/// `REF_TAG` heap handle — never raw primitive bits, so there is no collision
/// with wide-primitive payloads. `desc` only disambiguates Array vs object for a
/// tagged handle (`[` prefix ⇒ array).
pub(crate) fn unbox_ref(vm: &crate::Vm, cell: i64, desc: &str) -> JVal {
    // The descriptor is consumed for exactly ONE bit. Callers that already know it —
    // the compiled-entry boundary, which precomputes it per method — take
    // `unbox_ref_kind` and never touch the string.
    unbox_ref_kind(vm, cell, desc.as_bytes().first() == Some(&b'['))
}

/// [`unbox_ref`] with the array-vs-object question already answered. Split out because
/// `jit_enter` re-derived `ret_desc(descriptor).starts_with('[')` on every reference-
/// returning compiled entry — 7.6M entries a boot — for a bit that is a constant of the
/// method (see [`EntryPlan::ret_array`]).
pub(crate) fn unbox_ref_kind(vm: &crate::Vm, cell: i64, ret_array: bool) -> JVal {
    if cell == 0 {
        return JVal::Null;
    }
    let hi = (cell as u64) >> 32;
    if hi == BOX_TAG_HI {
        let _ = vm;
        let idx = (cell & 0xFFFF_FFFF) as usize;
        if let Some(v) = crate::frames::box_get(idx) {
            return v;
        }
    }
    if hi == REF_TAG_HI {
        let h = (cell & 0xFFFF_FFFF) as u32;
        return if ret_array { JVal::Array(h) } else { JVal::Ref(h) };
    }
    if hi == ARRAY_TAG_HI {
        return JVal::Array((cell & 0xFFFF_FFFF) as u32);
    }
    // Raw `ArenaPtr` arrival (next-gen-gc): `box_ref` passes `Value::Obj` through
    // unboxed, so the callback fallback decodes the bare pointer. A low-address
    // arena ptr is `< 2^40` (so `cell >> 40 == 0`), disjoint from every high-word
    // tag (`hi >= 256`) already handled above — never misreads a handle.
    #[cfg(feature = "next-gen-gc")]
    if cell != 0 && (cell as u64) >> 40 == 0 {
        // B5: a PINNED mirror address materializes as Ref(h) at the Rust boundary
        // (natives/dispatch stay handle-based during coexistence).
        if let Some(h) = crate::nextgen::pinned_handle_of(cell as u64) {
            return JVal::Ref(h);
        }
        return JVal::Obj(unsafe { crate::arena::ArenaPtr::from_addr(cell as u64) });
    }
    // Unreachable for a well-formed ref cell; a stray untagged value is treated
    // as a flat int defensively rather than panicking.
    JVal::Int(cell as i32)
}

/// Turn the raw i64 result bits back into a `Value` per the method's return type.
pub fn unpack_ret(ret: Option<JT>, bits: i64) -> Option<JVal> {
    match ret {
        Some(JT::Int) => Some(JVal::Int(bits as i32)),
        Some(JT::Long) => Some(JVal::Long(bits)),
        Some(JT::Float) => Some(JVal::Float(f32::from_bits(bits as u32))),
        Some(JT::Double) => Some(JVal::Double(f64::from_bits(bits as u64))),
        // The real ref-return path decodes through `unbox_ref` in `jit_run` (it
        // needs the VM box-table + return descriptor for Str/boxed-primitive and
        // array-vs-object). This vm-less arm is a defensive fallback that only
        // resolves bare tagged handles.
        Some(JT::Ref) => Some(match untag_ref(bits) {
            Some(h) => JVal::Ref(h),
            None => JVal::Null,
        }),
        None => None,
    }
}

// ── THE PER-METHOD ENTRY PLAN (kill switch `RAVA_NO_ENTRY_PLAN=1`) ──────────────────
//
// `Vm::jit_enter` runs 7,600,000 times in a product boot and is 4.6% of the compiled
// profile on its own. On every one of those entries it walked the DESCRIPTOR STRING to
// answer two questions that are CONSTANTS OF THE METHOD:
//
//   * which argument LOCAL SLOTS hold references (-> `box_ref` + the young-object read
//     barrier) and which hold primitives (-> flat `pack_local`), with a wide `J`/`D`
//     contributing a second, always-primitive slot; and
//   * whether the return type is an array (the single bit `unbox_ref` wanted the
//     descriptor for).
//
// That walk is a byte loop with a nested `L...;` scan inside it, run on a path whose whole
// job is a memcpy-sized argument marshal — the classification cost more than the copy it
// was classifying. This record answers both from 48 inline bytes built ONCE per compiled
// method.
//
// KEY = `JitFn::mid`, the interned printable identity stamped at the `jit_cache`
// publication point. It is the right key for three reasons and no other id in this build
// is: it is already in hand at entry (the very next lines push it for the sampler), it is
// dense from 1 (`register_compiled_method` returns `len()` after a push), and it is unique
// per compiled body BY CONSTRUCTION. Nothing here reads a class, method or package NAME —
// `mid` is an opaque counter, and the plan's contents come from the descriptor and the
// method's staticness, both of which are resolved dispatch facts.
//
// NOT DONE HERE, DELIBERATELY: nothing about the cell BUFFER. An allocation-free free list
// for it was built, gate-tested and measured as NOISE (min +0.79% / median -0.78%), and
// sizing it up front measured +3.42% at lane width 6; the cost this record removes is the
// CLASSIFICATION, not the malloc. (The buffer itself later moved onto the stack for small
// frames — see `INLINE_CELLS` — which is a different lever from either of those.)

/// Precomputed argument/return shape for one compiled method. Fixed width — 256 mask bits
/// covers the JVMS ceiling of 255 argument slots — so the plan needs no heap indirection
/// and the entry loop is a shift and a test per slot.
pub(crate) struct EntryPlan {
    /// Bit `i` set ⇒ argument local slot `i` is a REFERENCE slot. Slot 0 is the receiver
    /// for a virtual method; the high half of a wide param is always clear.
    ref_mask: [u64; 4],
    /// Argument local slots, receiver included, wide params counted twice.
    nslots: u16,
    /// `ret_desc(descriptor).starts_with('[')` — the whole reason the return path used to
    /// re-scan the descriptor tail.
    pub(crate) ret_array: bool,
    /// `ref_mask == [0; 4]` — NO argument slot of this method is a reference. Precomputed
    /// because it turns the marshal's per-slot, DATA-DEPENDENT ref-vs-prim branch into one
    /// per-METHOD branch that predicts perfectly (see [`marshal_cells`]). A non-static
    /// method always has the receiver in slot 0, so this implies `is_static`; the population
    /// is therefore static helpers whose parameters are all primitive (or none at all).
    all_prim: bool,
    /// Snapshot of the `RAVA_NO_FAST_ENTER3` gate, taken once when the plan is built.
    /// It lives HERE rather than being read at the entry because the entry has already
    /// loaded this cache line to reach `ret_array`/`nslots`, so consulting it costs a byte
    /// test instead of another process-constant gate read on a 7.6 M-entry path — the
    /// mistake the previous round removed three instances of.
    pub(crate) fast3: bool,
    /// Identity guard, checked on every lookup. `mid` is unique by construction, so this
    /// can only fire if that invariant is ever broken (a poisoned `METHOD_NAMES` mutex
    /// hands out 0, which is already excluded; a future id-recycling scheme would not be).
    /// A mismatch REFUSES — the caller falls through to the descriptor walk with no store,
    /// no allocation and no mutation, so the refusal is invisible.
    desc_len: u32,
    is_static: bool,
}

impl EntryPlan {
    /// The unpublished cell of a [`PlanSlot`]. EVERY FIELD IS THE ZERO BIT PATTERN, which is
    /// what keeps the 1 MB `PLAN_SLOTS` static a `zeroinitializer` — in `.bss`, demand-zero,
    /// absent from the binary. It is never read: a slot is only ever dereferenced after its
    /// `state` reads READY, and a blank slot's `state` is EMPTY. `desc_len: 0` is belt and
    /// braces — the shortest legal descriptor is `()V`, so even a read that should not exist
    /// would fail the identity guard and refuse.
    const BLANK: EntryPlan = EntryPlan {
        ref_mask: [0; 4],
        nslots: 0,
        ret_array: false,
        all_prim: false,
        fast3: false,
        desc_len: 0,
        is_static: false,
    };
    /// Number of argument local slots this method's cells cover.
    #[inline]
    pub(crate) fn nslots(&self) -> usize {
        self.nslots as usize
    }
    /// Is argument local slot `i` a reference slot? `i < nslots() <= 256`, so the word
    /// index is in range without a bounds branch of its own.
    #[inline]
    pub(crate) fn is_ref(&self, i: usize) -> bool {
        self.ref_mask[i >> 6] & (1u64 << (i & 63)) != 0
    }
}

/// Build the plan by walking the descriptor exactly the way `jit_enter` used to, so the
/// slot sequence is byte-identical to the path it replaces (including the degenerate
/// "trailing `[` with no element type" arm, which classifies as one primitive slot).
/// `None` = do not memoize: more argument slots than the mask can hold, which the JVMS
/// forbids and which therefore only arises on a malformed descriptor.
fn build_entry_plan(descriptor: &str, is_static: bool) -> Option<EntryPlan> {
    let mut ref_mask = [0u64; 4];
    let mut n = 0usize;
    if !is_static {
        ref_mask[0] |= 1; // receiver: always a reference slot, always slot 0
        n = 1;
    }
    let b = descriptor.as_bytes();
    let mut di = crate::descriptor::params_start(descriptor);
    while di < b.len() && b[di] != b')' {
        let is_array = b[di] == b'[';
        while di < b.len() && b[di] == b'[' {
            di += 1;
        }
        let kind = if di < b.len() { b[di] } else { b')' };
        // `[J`/`[D` are NARROW references — the wide test must exclude arrays, the trap
        // `arg_slots` documents and that this tree has already been bitten by once.
        let wide = !is_array && matches!(kind, b'J' | b'D');
        let is_ref = is_array || kind == b'L';
        if kind == b'L' {
            while di < b.len() && b[di] != b';' {
                di += 1;
            }
        }
        di += 1;
        if n >= 256 {
            return None;
        }
        if is_ref {
            ref_mask[n >> 6] |= 1u64 << (n & 63);
        }
        n += 1;
        if wide {
            if n >= 256 {
                return None;
            }
            n += 1; // high half of a wide param: packs flat, never a root
        }
    }
    Some(EntryPlan {
        ref_mask,
        nslots: n as u16,
        ret_array: crate::descriptor::ret_desc(descriptor).as_bytes().first() == Some(&b'['),
        all_prim: ref_mask == [0u64; 4],
        fast3: plan_mode() == MODE_FAST3,
        desc_len: descriptor.len() as u32,
        is_static,
    })
}

/// No plan at all: `RAVA_NO_ENTRY_PLAN=1` (or `emit_more_disabled`). Every entry re-walks
/// the descriptor and re-resolves the per-op thread-locals, so one boot bisects the whole
/// entry rework rather than half of it.
const MODE_OFF: u8 = 0;
/// Plans, but served from the ORIGINAL per-thread `RefCell<Vec<..>>` table, and none of the
/// entry-side skips. `RAVA_NO_FAST_ENTER3=1` selects this.
const MODE_TLS: u8 = 1;
/// Default: plans served from the process-global lock-free table, plus the entry skips.
const MODE_FAST3: u8 = 2;

/// One process-constant read for BOTH switches, so the hot path pays a single `OnceLock`
/// probe rather than one per gate. `RAVA_NO_ENTRY_PLAN` dominates: with no plan there is
/// nothing for `RAVA_NO_FAST_ENTER3` to switch off.
fn plan_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        if crate::flags::emit_more_disabled() || std::env::var_os("RAVA_NO_ENTRY_PLAN").is_some() {
            MODE_OFF
        } else if std::env::var_os("RAVA_NO_FAST_ENTER3").is_some() {
            MODE_TLS
        } else {
            MODE_FAST3
        }
    })
}

thread_local! {
    /// `mid - 1` -> plan, the ORIGINAL per-thread table. Kept as the `RAVA_NO_FAST_ENTER3`
    /// arm only; the default path is [`PLANS`] below.
    /// APPEND-ONLY: a slot is written once and never cleared or overwritten.
    static ENTRY_PLANS: std::cell::RefCell<Vec<Option<Box<EntryPlan>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// ── THE POINTER PLAN TABLE — NOW THE `RAVA_NO_SLOTPOOL=1` ARM ───────────────────────────
///
/// This is the shape the default path REPLACED (see [`PLAN_SLOTS`] below); it is kept
/// verbatim so `RAVA_NO_SLOTPOOL=1` restores it, and it is still what `RAVA_NO_FAST_ENTER3`
/// switches off. The record of why it beat the thread-local follows unchanged.
///
/// The per-thread `RefCell<Vec<Option<Box<EntryPlan>>>>` above was the right first shape —
/// it avoided a lock on a path taken 7.6 M times — but it charged the WRONG price for that.
/// Every compiled entry paid: a `LocalKey::with` crossing (a lazy-init state check, and on
/// Windows the TLS-destructor registration test), a `RefCell::borrow_mut` (a flag load, a
/// compare against the panic case, a store, and a second store when the `Ref` drops), a
/// `Vec::len` bound test, and TWO bounds-checked indexes — all to reach a pointer that
/// never changes once written.
///
/// The lock the comment feared is not the only alternative to a thread-local. A plan is
/// IMMUTABLE once built and identical for every thread (`mid` names one compiled body, so
/// the descriptor and staticness it is derived from are the same everywhere), which makes
/// the table a publish-once pointer array: a single `Acquire` load on the hot path (a plain
/// `mov` on x86-64), no crossing and no borrow flag. Two threads racing to build the same `mid` both
/// build; the `compare_exchange` picks one and the loser frees its own `Box` immediately,
/// so the race costs a redundant descriptor walk and nothing else.
///
/// SIZE. `mid` is dense from 1 (`register_compiled_method` returns `len()` after a push),
/// so entry `mid - 1` is the natural index and the live prefix is contiguous. 16 384 slots
/// is 128 KB of BSS — demand-zero, so only the pages a boot actually reaches are ever
/// faulted in (~1.5-2 k compiled bodies = 16 KB = four pages), and the previous round's
/// lesson about a cache that stopped fitting does not apply to a table whose working set is
/// the pointers of the hot methods themselves. A `mid` past the end simply gets `None` and
/// takes the descriptor walk, which is the same correct fallback as an unpublished id.
const PLAN_TABLE_LEN: usize = 16384;
static PLANS: [std::sync::atomic::AtomicPtr<EntryPlan>; PLAN_TABLE_LEN] =
    [const { std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()) }; PLAN_TABLE_LEN];

/// ── THE PLAN LIVES IN THE TABLE, NOT BEHIND IT (kill switch `RAVA_NO_SLOTPOOL=1`) ───────
///
/// THE MEASUREMENT THIS ANSWERS. A boot profile puts `entry_plan` at 0.8% of host samples on
/// its own. It cannot be its arithmetic: the served path is a bounds test, one atomic load, a
/// null test and two field compares — call it a dozen instructions, which over ~7.6 M entries
/// is a tenth of that share. What it can be, and what the shape says it is, is MEMORY. The
/// hot path was TWO DEPENDENT LOADS in TWO SEPARATE REGIONS: `PLANS[mid-1]` (a 128 KB pointer
/// array) and then the `Box<EntryPlan>` it names — a ~48-byte `mi_malloc` allocation sitting
/// wherever the allocator put it when this method was first entered, interleaved with every
/// other allocation the compile made. The second load cannot start until the first retires,
/// and neither address is predictable from the one before, so a cold method pays two
/// misses in series to read 48 bytes of immutable data whose index we already had.
///
/// THE FIX IS THE OBVIOUS ONE ONCE IT IS STATED THAT WAY: put the plan IN the slot. `mid` is
/// dense from 1, the plan is immutable once built, it is the same on every thread, and it is
/// never freed — every property that made the pointer table publish-once also makes the
/// pointer itself unnecessary. One `Acquire` load of the slot's own publication byte, then
/// the fields, FROM THE SAME CACHE LINE. No indirection, and no `Box` — the malloc and the
/// free-list traffic behind it disappear with the pointer, along with the race's `drop`.
///
/// THE PUBLICATION PROTOCOL, and why a shared mutable static is sound here.
///   * `state` is the only synchronising word: `EMPTY -> BUILDING -> READY`, never back
///     (except the unbuildable-descriptor rollback below, which is `BUILDING -> EMPTY` by the
///     same thread that took it and before any reader can be admitted).
///   * A READER is admitted only by `state == READY` under `Acquire`, which orders the
///     builder's `Release` store after its field writes. Nothing writes a READY slot ever
///     again, so the `&'static EntryPlan` handed out is immutable for the life of the
///     process and sharing it across threads is sound.
///   * A WRITER is admitted only by winning `compare_exchange(EMPTY -> BUILDING)`, so at most
///     one thread ever writes a given slot. A racing thread that finds BUILDING REFUSES
///     (returns `None`) — the caller takes the descriptor walk, which produces the identical
///     cells (see [`marshal_cells`]'s byte-identity note), and the next entry finds READY.
///     A guard that only ever refuses cannot admit a torn read.
///
/// SIZE, honestly. `#[repr(C, align(64))]` makes one slot exactly one cache line, so a
/// lookup is guaranteed to touch ONE line rather than straddling two — the whole point of
/// the change. 16 384 slots is therefore 1 MB of address space against the old table's
/// 128 KB, and this is the round that learned "a cache that stopped fitting is worse than the
/// allocation it removes", so the numbers are worth stating rather than waving at. It is not
/// that kind of table: it is not a cache (nothing collides, nothing is evicted, the index IS
/// the identity), the initialiser is ALL ZERO so it is `.bss` and demand-zero — untouched
/// pages are never faulted in — and the live prefix for ~1.5 k compiled bodies is 96 KB,
/// against the 12 KB of pointers plus ~72 KB of scattered `Box`es it replaces. Same working
/// set, one region instead of two, one line instead of two lines.
///
/// `RAVA_NO_SLOTPOOL=1` (or `emit_more_disabled()`) leaves every slot at EMPTY forever, so
/// the probe below always falls through to [`entry_plan_cold`], which runs the historic
/// pointer-table code above verbatim. That arm pays one extra load of a table it never
/// populates; it is a bisect handle, not a second product path.
#[repr(C, align(64))]
struct PlanSlot {
    /// EMPTY / BUILDING / READY. The only word two threads ever race on.
    state: std::sync::atomic::AtomicU8,
    /// Valid — and only ever read — while `state == READY`.
    plan: std::cell::UnsafeCell<EntryPlan>,
}

/// SAFETY: the `UnsafeCell` is written exactly once, by the single thread that won the
/// `EMPTY -> BUILDING` transition, and is read only after an `Acquire` load observes the
/// matching `Release` store of READY. After that it is immutable for the life of the process,
/// so `&PlanSlot` carries no interior mutation a reader can observe.
unsafe impl Sync for PlanSlot {}

const PLAN_EMPTY: u8 = 0;
const PLAN_BUILDING: u8 = 1;
const PLAN_READY: u8 = 2;

impl PlanSlot {
    /// ALL-ZERO by construction — see the `.bss` note above. Every field of
    /// [`EntryPlan::BLANK`] is zero too, so the whole 1 MB static is a `zeroinitializer` and
    /// costs nothing in the binary and nothing in RSS until a page is written.
    const fn empty() -> Self {
        PlanSlot {
            state: std::sync::atomic::AtomicU8::new(PLAN_EMPTY),
            plan: std::cell::UnsafeCell::new(EntryPlan::BLANK),
        }
    }
}

static PLAN_SLOTS: [PlanSlot; PLAN_TABLE_LEN] = [const { PlanSlot::empty() }; PLAN_TABLE_LEN];

/// `RAVA_NO_SLOTPOOL=1` — publish plans through the historic `Box` + `AtomicPtr` table
/// instead of inline in [`PLAN_SLOTS`]. Read ONCE per process and only from the COLD path:
/// the served path decides nothing from it (an unpopulated slot is simply not READY), which
/// is the point — the previous round's finding was that per-entry process-constant gate READS
/// are themselves the cost, and this gate now never appears on the 7.6 M-entry path at all.
fn slotpool_off() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        crate::flags::emit_more_disabled() || std::env::var_os("RAVA_NO_SLOTPOOL").is_some()
    })
}

/// The plan for `mid`, built on first sight and memoized thereafter.
/// `None` ⇒ the caller must take the descriptor walk: the method has no interned id
/// (`mid == 0`, i.e. a body published before `register_compiled_method` ran), the switch
/// is off, the id is past the table, another thread is mid-build, or the identity guard
/// refused. Every one of those is a REFUSAL: no store, no allocation, no mutation, and the
/// walk produces the identical cells, so a refusal is invisible except in speed.
///
/// SERVED PATH ONLY, and inline on purpose: bounds test, one `Acquire` byte load, one
/// compare, two field compares — all against ONE cache line, with no call, no `OnceLock`
/// probe and no second dereference. Everything else, including all three kill-switch arms
/// and the first-sight build, is out of line in [`entry_plan_cold`], so the served path does
/// not carry their registers.
///
/// The `'static` is honest on all three arms. Slot arm: the slot is a `static` and is never
/// rewritten after READY. Pointer arm: the `Box` is published by leaking it (`Box::into_raw`)
/// and the slot is never cleared, so the pointee outlives the process. Thread-local arm: the
/// table is append-only, entries are never removed or replaced, and the caller consumes the
/// reference entirely inside one `jit_enter` — re-entry may grow the `Vec`, which moves the
/// `Box` POINTERS, not the `EntryPlan`s they own.
#[inline]
pub(crate) fn entry_plan(
    mid: u32,
    descriptor: &str,
    is_static: bool,
) -> Option<&'static EntryPlan> {
    if mid == 0 {
        return None;
    }
    let idx = mid as usize - 1;
    if idx < PLAN_TABLE_LEN {
        let slot = &PLAN_SLOTS[idx];
        if slot.state.load(std::sync::atomic::Ordering::Acquire) == PLAN_READY {
            // SAFETY: READY is stored with `Release` only after the single admitted builder
            // finished writing the cell, and no thread ever writes the cell again; the
            // `Acquire` above orders those writes before every read here.
            let p = unsafe { &*slot.plan.get() };
            if p.desc_len != descriptor.len() as u32 || p.is_static != is_static {
                return None;
            }
            return Some(p);
        }
    }
    entry_plan_cold(idx, descriptor, is_static)
}

/// First sight, the two kill-switch arms, and a `mid` past the table. Out of line so the
/// served path above keeps its registers; `#[cold]` because on a product boot this runs once
/// per compiled body against ~7.6 M served entries.
#[cold]
#[inline(never)]
fn entry_plan_cold(
    idx: usize,
    descriptor: &str,
    is_static: bool,
) -> Option<&'static EntryPlan> {
    let mode = plan_mode();
    if mode == MODE_OFF {
        return None;
    }
    if mode == MODE_FAST3 {
        if idx >= PLAN_TABLE_LEN {
            return None;
        }
        if slotpool_off() {
            return entry_plan_boxed(idx, descriptor, is_static);
        }
        use std::sync::atomic::Ordering::{Acquire, Release};
        let slot = &PLAN_SLOTS[idx];
        match slot.state.compare_exchange(PLAN_EMPTY, PLAN_BUILDING, Acquire, Acquire) {
            Ok(_) => {
                let built = match build_entry_plan(descriptor, is_static) {
                    Some(p) => p,
                    // A descriptor the mask cannot hold (JVMS-illegal). Hand the slot back
                    // rather than wedging it at BUILDING for the life of the process — no
                    // reader has been admitted to it, so this is invisible.
                    None => {
                        slot.state.store(PLAN_EMPTY, Release);
                        return None;
                    }
                };
                // SAFETY: this thread is the only one admitted to write this cell (it won the
                // EMPTY -> BUILDING transition, readers require READY and writers require
                // EMPTY), and the `Release` store below publishes the write.
                unsafe { slot.plan.get().write(built) };
                slot.state.store(PLAN_READY, Release);
            }
            // Published between the probe and here — fall through and read it.
            Err(PLAN_READY) => {}
            // BUILDING: another thread owns this slot right now. REFUSE; the caller walks the
            // descriptor and the next entry finds READY.
            Err(_) => return None,
        }
        // SAFETY: as above — READY was observed (or stored by us) with the matching ordering.
        let p = unsafe { &*slot.plan.get() };
        if p.desc_len != descriptor.len() as u32 || p.is_static != is_static {
            return None;
        }
        return Some(p);
    }
    entry_plan_tls(idx, descriptor, is_static)
}

/// `RAVA_NO_SLOTPOOL=1`: the historic leaked-`Box` + `AtomicPtr` table, verbatim.
fn entry_plan_boxed(
    idx: usize,
    descriptor: &str,
    is_static: bool,
) -> Option<&'static EntryPlan> {
    use std::sync::atomic::Ordering::{Acquire, Release};
    let slot = &PLANS[idx];
    let mut p = slot.load(Acquire);
    if p.is_null() {
        // Not published yet on any thread. Build ours, then try to publish it.
        let fresh = Box::into_raw(Box::new(build_entry_plan(descriptor, is_static)?));
        match slot.compare_exchange(std::ptr::null_mut(), fresh, Release, Acquire) {
            Ok(_) => p = fresh,
            Err(won) => {
                // Another thread published the identical plan first. Free ours here —
                // it was never reachable by anyone else, so no other thread can hold it.
                // SAFETY: `fresh` came from `Box::into_raw` on this thread and the
                // `compare_exchange` failed, so it was never stored anywhere.
                drop(unsafe { Box::from_raw(fresh) });
                p = won;
            }
        }
    }
    // SAFETY: a non-null slot was published by a `Release` store of a leaked `Box` and
    // is never cleared; the matching `Acquire` load above orders that `Box`'s
    // initialising writes before every read of it here. `EntryPlan` is plain immutable
    // data, so sharing `&` across threads is sound.
    let p = unsafe { &*p };
    if p.desc_len != descriptor.len() as u32 || p.is_static != is_static {
        return None;
    }
    Some(p)
}

/// `RAVA_NO_FAST_ENTER3=1`: the original per-thread table, verbatim.
fn entry_plan_tls(
    idx: usize,
    descriptor: &str,
    is_static: bool,
) -> Option<&'static EntryPlan> {
    let p: *const EntryPlan = ENTRY_PLANS.with(|c| -> Option<*const EntryPlan> {
        let mut t = c.borrow_mut();
        if t.len() <= idx {
            t.resize_with(idx + 1, || None);
        }
        if t[idx].is_none() {
            t[idx] = Some(Box::new(build_entry_plan(descriptor, is_static)?));
        }
        t[idx].as_ref().map(|b| &**b as *const EntryPlan)
    })?;
    // SAFETY: see the doc comment — append-only table, stable `Box` pointee, consumed
    // only within the calling `jit_enter`.
    let p = unsafe { &*p };
    if p.desc_len != descriptor.len() as u32 || p.is_static != is_static {
        return None;
    }
    Some(p)
}

// ── THE CALL-TRANSITION MARSHAL (kill switch `RAVA_NO_FAST_STUB=1`) ────────────────────
//
// WHAT THIS IS AND WHAT IT IS NOT. The seam into compiled code was asked to be rewritten as
// a hand-written call stub, the way HotSpot writes one. It is NOT one, and the reason is a
// measurement of the machine code rather than a preference: `jit::call_raw` already lowers to
// `mov rcx, buf; mov rdx, desc_cell; call rax` — two register arguments in the Windows x64
// integer registers and one indirect call. There is no argument shuffling to hand-schedule,
// no callee-saved set to elide (the callee is Cranelift-generated and saves its own), and a
// `global_asm!` trampoline could only ADD a call layer between the Rust frame and the guest.
// The cost at this seam is not the transition instruction sequence; it is the BOOKKEEPING
// around it, and that is what this half does:
//
//   * ONE buffer, allocated at its exact final length, instead of `with_capacity` + N pushes
//     + a `resize` that had to zero the spill tail afterwards. For a frame that fits inline
//     there is NO allocation at all: the cells live in `jit_enter`'s own stack frame, which
//     removes a malloc/free PAIR from a path a product boot takes ~7.6 M times.
//   * ONE marshalling implementation for both the planned and the descriptor-walk arms, so
//     the two can no longer drift, writing cells BY INDEX into a pre-zeroed buffer.
//
// BYTE-IDENTITY. The old code pushed cells and then called `cells.resize(total, 0)`, which
// both zero-extends AND truncates — so the buffer handed to compiled code was ALWAYS exactly
// `total` cells, with cell `i` holding the packed local `i` for `i < min(pushed, total)` and
// 0 above it. Pre-zeroing `total` cells and writing `cells[i]` for `i < total` produces that
// same array, cell for cell, for every length including `total == 0` and for the degenerate
// case where a caller passes more locals than the frame declares. The per-slot classification
// and the packing calls are moved verbatim, in the same order, so `box_ref`'s side effect on
// the box table happens in the same sequence with the same indices.

/// One reference-slot argument cell: `box_ref` plus the young-object read barrier.
///
/// The barrier corrects a stale receiver/argument that an in-flight call buffer carried
/// across a collection (follow the evacuated slot's forwarding word). REFERENCE slots only —
/// a primitive's bits are not a pointer, so applying it there would reinterpret an integer.
#[inline]
fn ref_cell(vm: &crate::Vm, v: &JVal) -> i64 {
    let c = box_ref(vm, v);
    #[cfg(feature = "next-gen-gc")]
    let c = crate::nextgen::forward_if_moved(c);
    c
}

/// The descriptor-walk arm, kept byte-for-byte as `jit_enter` wrote it and pushed OUT OF LINE.
/// It runs only when there is no [`EntryPlan`] (kill switch on, or a body published before
/// `register_compiled_method` stamped a `mid`), which on a product boot is a rounding error —
/// leaving it inline cost the hot arm the descriptor pointer, the walk cursor and the
/// classification temporaries in registers `jit_enter` needs for the pack itself.
///
/// Returns the next unfilled slot index, exactly as the inline walk left `slot`.
#[inline(never)]
fn marshal_walk(
    vm: &crate::Vm,
    descriptor: &str,
    is_static: bool,
    locals: &[JVal],
    cells: &mut [i64],
) -> usize {
    let n_cells = cells.len();
    let mut slot = 0usize;
    // Store only inside the buffer: a cell past `total` was pushed and then dropped by the
    // old `resize` truncation, so writing it was already dead. `ref_cell` still RUNS for such
    // a slot, because its `box_push` is observable in the box table's length.
    macro_rules! put {
        ($c:expr) => {{
            let c = $c;
            if slot < n_cells {
                cells[slot] = c;
            }
            slot += 1;
        }};
    }
    if !is_static {
        if let Some(v) = locals.get(slot) {
            put!(ref_cell(vm, v));
        }
    }
    let dbytes = descriptor.as_bytes();
    let mut di = crate::descriptor::params_start(descriptor);
    while di < dbytes.len() && dbytes[di] != b')' {
        let is_array = dbytes[di] == b'[';
        while di < dbytes.len() && dbytes[di] == b'[' {
            di += 1;
        }
        let kind = if di < dbytes.len() { dbytes[di] } else { b')' };
        let wide = !is_array && matches!(kind, b'J' | b'D');
        let is_ref = is_array || kind == b'L';
        if kind == b'L' {
            while di < dbytes.len() && dbytes[di] != b';' {
                di += 1;
            }
        }
        di += 1;
        if let Some(v) = locals.get(slot) {
            put!(if is_ref { ref_cell(vm, v) } else { pack_local(v) });
        }
        if wide {
            if let Some(v) = locals.get(slot) {
                put!(pack_local(v));
            }
        }
    }
    slot
}

/// Fill `cells` (already zeroed, length = `max_locals + max_stack`) with the packed argument
/// locals, then leave the GC spill tail at zero.
///
/// PLANNED ARM: no descriptor bytes are read. One shift-and-test per argument slot decides
/// ref-vs-prim; wide params are already expanded into two slots (the high half classified
/// primitive) by `build_entry_plan`, and the receiver is simply slot 0 of a non-static
/// method's mask. Termination matches the walk it replaces exactly: there, a `locals.get(slot)`
/// miss left `slot` unchanged, so every later `get` missed too and no further cell was
/// pushed — stopping at `min(nslots, locals.len())` is the same sequence.
///
/// TAIL: locals beyond the declared argument slots pack flat, as before. `pack_local` is a
/// pure function of the `Value`, so a tail slot that would land past the end of the buffer
/// (and be truncated away) is simply not computed.
#[inline]
pub(crate) fn marshal_cells(
    vm: &crate::Vm,
    plan: Option<&EntryPlan>,
    descriptor: &str,
    is_static: bool,
    locals: &[JVal],
    cells: &mut [i64],
) {
    let n_cells = cells.len();
    let slot = match plan {
        Some(p) => {
            let n = p.nslots().min(locals.len());
            let mut i = 0usize;
            // ── SKIP 2 (kill switch `RAVA_NO_FAST_ENTER3=1`): NO REFERENCE ARGUMENTS ⇒ NO
            // `box_ref` LOOP. ──────────────────────────────────────────────────────────────
            // The mixed loop below asks `p.is_ref(i)` per slot — a word load out of the plan,
            // a shift, a mask, and then a branch whose direction is DATA (which parameter is
            // which), so a method with mixed argument kinds mispredicts it every time the
            // pattern changes. When the plan says the whole mask is zero there is nothing to
            // ask: every slot packs flat, the branch disappears, and `ref_cell` — with its
            // `box_ref` match and its `forward_if_moved` read barrier — is not reached at all.
            //
            // BYTE-IDENTITY: `all_prim` is exactly `ref_mask == [0; 4]`, so the mixed loop
            // would have taken the `pack_local` arm for every one of these slots and
            // `ref_cell` would never have run — meaning no `box_push`, so the box table's
            // length and contents are unchanged too. Same cells, same side effects, same
            // order, same terminating `i`.
            //
            // POPULATION: a non-static method always has its receiver in slot 0, which is
            // always a reference, so this arm is reached only by STATIC methods whose
            // parameters are all primitive — arithmetic and hash/compare helpers — plus the
            // degenerate static no-arg case, where both loops are empty and the win is nil.
            if p.fast3 && p.all_prim {
                while i < n {
                    let c = pack_local(&locals[i]);
                    if i < n_cells {
                        cells[i] = c;
                    }
                    i += 1;
                }
                return finish_tail(locals, cells, i);
            }
            while i < n {
                let v = &locals[i];
                let c = if p.is_ref(i) { ref_cell(vm, v) } else { pack_local(v) };
                if i < n_cells {
                    cells[i] = c;
                }
                i += 1;
            }
            i
        }
        None => marshal_walk(vm, descriptor, is_static, locals, cells),
    };
    finish_tail(locals, cells, slot);
}

/// The flat tail of [`marshal_cells`], shared with the all-primitive arm so the two cannot
/// drift. Locals beyond the declared argument slots pack flat; a slot that would land past
/// the end of the buffer was truncated away by the old `resize` and so is not computed.
#[inline]
fn finish_tail(locals: &[JVal], cells: &mut [i64], mut slot: usize) {
    let tail = locals.len().min(cells.len());
    while slot < tail {
        cells[slot] = pack_local(&locals[slot]);
        slot += 1;
    }
}

/// `RAVA_NO_FAST_STUB=1` — routes the compiled-entry boundary back to a HEAP cell buffer on
/// every entry (never the inline stack one) AND back to the two separate thread-local
/// resolutions of the active `JitCtx` (`array_desc_cell_ptr` then `clear_jit_fault`), so one
/// boot bisects the whole call-transition rework rather than half of it. The marshalling
/// itself is not switched: both arms run [`marshal_cells`], which reproduces the old
/// push-then-`resize` buffer cell for cell.
pub(crate) fn fast_stub_off() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        crate::flags::emit_more_disabled() || std::env::var_os("RAVA_NO_FAST_STUB").is_some()
    })
}

/// Cells that fit in `jit_enter`'s own stack frame instead of a heap buffer.
///
/// 32 cells = 256 bytes of stack, which covers `max_locals + max_stack` for the overwhelming
/// majority of compiled bodies (a typical accessor or arithmetic helper declares fewer than a
/// dozen of each). Above it the heap arm takes over, so the ceiling is a size trade and never
/// a correctness one. Deliberately modest: `jit_enter` sits on a recursive host↔guest chain,
/// so this array is paid once per nesting level whether or not the inline arm is taken.
pub(crate) const INLINE_CELLS: usize = 32;
