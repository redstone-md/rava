//! Default-build (no `next-gen-gc`) no-op stand-in for the arena layer: exactly the
//! surface the shared (ungated) JIT/diagnostic call sites touch. Without the feature
//! no arena exists and nothing ever moves, so the barrier is identity and every
//! arena probe is empty/false — the default build's behavior is unchanged.
//!
//! Next-gen arena/nursery allocator (integration foundation, gated OFF by default).
//!
//! Next-gen GC integration foundation — gated by `#[cfg(feature = "next-gen-gc")]`,
//! default OFF so the production engine is byte-identical.
//!
//! This is the in-tree port of the bump-arena + generational copying nursery
//! proven in `crates/sandbox-gc` (Allocs 0.87x JVM single-thread; up to 4x via
//! per-thread TLABs; scalar replacement removes non-escaping allocs entirely).
//!
//! WIRING PLAN (staged, each behind this flag, each attended + boot-validated):
//!   1. TLAB: a `thread_local!` `Nursery` per OS worker thread → `new_object`
//!      becomes a thread-local pointer bump (zero sync on the hot path).
//!   2. GC roots: the JIT spills live object pointers (already done for the
//!      handle model via `spill_for_gc`); the copying collector walks those + the
//!      interpreter operand stacks/locals as the root set, forwarding each `*mut`.
//!   3. Write barrier + remembered set: MC has old→young field stores (unlike the
//!      clean newer→older sandbox workloads), so `putfield` of a young ref into a
//!      tenured object records the slot in a per-region card table.
//!   4. Escape analysis: a Cranelift e-graph pass tags allocs whose handle never
//!      escapes the compiled method → scalar-replace (fields → SSA values), the
//!      object never reaches the arena.
//!
//! Until wired, this module is self-contained dead code: it lets the flag compile
//! and gives the integration its allocator without touching the live `Heap`.















use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Object header in the arena (repr C, 8-byte aligned): [0]=class u32,
/// [4]=nfields u32, [8]=forwarding ptr, [16]=identity-hash word (0 = unset,
/// lazily stamped on first System.identityHashCode), [24..]=inline i64 cells.
/// The hash lives IN the header, so the Cheney evacuator (which copies `obj_size`
/// bytes) carries it to tenured automatically — a stamped hash is GC-stable.
///
/// Under `shrink-header` the layout narrows to 16B: [0]=class u32, [4]=nfields u32,
/// [8]=forwarding OFFSET u32, [12]=identity hash u32, [16..]=cells. Bytes [8,16) are
/// exactly what the 24B form wastes — a forwarding target only ever names an 8-aligned
/// arena address, so it needs 32 bits (not 64) once stored relative to `ARENA_FLOOR`,
/// and the 24B form additionally pads 4 bytes after the u32 hash.
///
/// This is deliberately NOT the textbook "overlay the forwarding pointer on the
/// header" trick. That is REFUTED here: `arena_obj_header_plausible` (and through it
/// `copy_obj_guarded`) re-reads class+nfields on an ALREADY-FORWARDED object at every
/// evacuation edge, `forward_stale_arena` resolves a field slot from the class id of a
/// possibly-moved receiver, and `stale_young_ptr` sanity-checks `hdr >> 32`. Destroying
/// class/nfields would make the second visit to a forwarded object read as implausible
/// and silently return the DANGLING from-space pointer. Narrowing the word instead
/// keeps class+nfields intact and keeps 0 meaning "not forwarded".
#[cfg(not(feature = "shrink-header"))]
pub const HDR: usize = 24;
#[cfg(feature = "shrink-header")]
pub const HDR: usize = 16;

/// Identity-hash word offset inside the header.
#[cfg(not(feature = "shrink-header"))]
const HASH_OFF: usize = 16;
#[cfg(feature = "shrink-header")]
const HASH_OFF: usize = 12;

/// The lowest address any arena region can occupy — `NEXT_LOW`'s start (8 GiB). Every
/// region is mapped at `>= ARENA_FLOOR` and `< LOW_CEILING`, 8-aligned (see the
/// `map_low_zeroed` assert). Below the floor lies the small-integer / null-page range.
pub const ARENA_FLOOR: u64 = 8 * 1024 * 1024 * 1024;

unsafe impl Send for PinnedHeap {}
unsafe impl Sync for PinnedHeap {}

/// Bump offset (bytes past `base`) into the pinned region. The single GIL writer
/// advances it with `fetch_add`; `is_pinned` readers observe progress via `PINNED_TOP`,
/// not this. Stays 0 until the first `pinned_alloc` (region unmapped ⇒ canonical
/// bit-exact on any run with no pinned object).
static PINNED_OFF: AtomicUsize = AtomicUsize::new(0);

/// Live [base, top) of the pinned region, mirrored for a lock-free `is_pinned`.
/// `base == 0` until the region is first mapped, so `is_pinned` is a single relaxed
/// load returning false on any run that never allocates a pinned object.
static PINNED_BASE: AtomicU64 = AtomicU64::new(0);
static PINNED_TOP: AtomicU64 = AtomicU64::new(0);

/// Reserved VA for the pinned region (committed on demand via `Region::ensure`).
const PINNED_CAP: usize = 256 * 1024 * 1024;

/// handle (u32) → pinned mirror address (0 = none). Parallel to `Heap::objects`;
/// a swept+reused handle is overwritten at the reused slot's next alloc, so it
/// always names the CURRENT object's mirror (the old mirror leaks, like arena_box —
/// acceptable at boot scale until a later brick reclaims pinned space).
///
/// LOCK-FREE grow-only CHUNKED array of `AtomicU64` (was `Mutex<Vec<u64>>`): the
/// mutex sat on the HOTTEST field path (every read + every write, even for the vast
/// majority of objects that have no mirror), costing the arc its +1.5s SETUP tax.
/// Growth allocates a new fixed-size chunk and NEVER reallocates/moves existing data
/// — so a lock-free reader can hold a chunk pointer while the writer grows a
/// different chunk with no ABA/dangling hazard (a plain `Vec` realloc behind an
/// atomic pointer would invalidate concurrent readers). A single guest writer holds
/// the GIL (workers never allocate pinned — see `pinned_alloc`), so `set` needs no
/// CAS: it is the only mutator of the directory + cells. Readers are lock-free.
///
/// Directory covers the full u32 handle space: `PMM_CHUNK` (2^16) entries per chunk,
/// `PMM_DIR_LEN` (2^16) chunk pointers → 512 KiB zero-init BSS static, demand-paged
/// (canonical never allocates a mirror ⇒ no chunk mapped ⇒ bit-exact).
const PMM_CHUNK_BITS: u32 = 16;
const PMM_CHUNK: usize = 1 << PMM_CHUNK_BITS; // 65_536 handles/chunk
const PMM_CHUNK_MASK: u32 = (PMM_CHUNK as u32) - 1;
const PMM_DIR_LEN: usize = 1 << (32 - PMM_CHUNK_BITS); // 65_536 chunks span u32

/// Chunk directory: `PMM_DIR[ci]` is null until handle-range `ci` is first written.
/// Published Release / read Acquire so a lock-free reader that observes a non-null
/// chunk pointer also observes the chunk's zeroed cells (no uninitialized read).
static PMM_DIR: [AtomicPtr<AtomicU64>; PMM_DIR_LEN] =
    [const { AtomicPtr::new(std::ptr::null_mut()) }; PMM_DIR_LEN];

/// One past the highest chunk index ever allocated — bounds the GC's `ref_cells`
/// scan so an empty directory costs zero iterations (updated under the GIL in `set`).
static PMM_DIR_USED: AtomicUsize = AtomicUsize::new(0);

const STR_BLEN_OFF: usize = HDR; // 24 — byte length
const STR_CLEN_OFF: usize = HDR + 4; // 28 — char length (== Java length())
const STR_HASH_OFF: usize = HDR + 8; // 32 — cached Java hashCode
const STR_BYTES_OFF: usize = HDR + 16; // 40 — inline UTF-8 bytes

/// JIT-facing offsets for inline String intrinsics (`[obj + off]`).
pub const STR_JIT_CLEN_OFF: i32 = STR_CLEN_OFF as i32;
pub const STR_JIT_HASH_OFF: i32 = STR_HASH_OFF as i32;
pub const STR_JIT_BLEN_OFF: i32 = STR_BLEN_OFF as i32;
pub const STR_JIT_BYTES_OFF: i32 = STR_BYTES_OFF as i32;
unsafe impl Send for PermPool {}

/// Total arena-lambda allocations this run (unconditional — one relaxed increment on a
/// path that already does a bump allocation, so it needs no gate and cannot be a
/// zero-because-unarmed reading).
pub static ARENA_LAMBDA_BIRTHS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Row 77: how often the `u32`-mask scan meets an object whose cell count exceeds the mask's
/// width, and the largest count it ever saw. Non-zero here means the wrap is REACHABLE and
/// there is a tracing bug independent of the array job.
pub static SHAPE_WIDE_SCANS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SHAPE_MAX_NF: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Births by CAPTURE ARITY (index = arity, last bucket = 8+). Both dedup maps key only on
/// `dyn_args.len() == 1`, and the arena twin removed just 2.4% of births — so the covered
/// slice is small and the question is what the uncovered majority actually looks like.
/// A count, not a timing, so a contended box cannot corrupt it.
pub static ARENA_LAMBDA_ARITY: [std::sync::atomic::AtomicU64; 9] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 9];

const AL_KIND_SLOT: usize = 0;
pub(crate) const AL_SITE_SLOT: usize = 1;
const AL_FI_SLOT: usize = 2;
const AL_CMP_SLOT: usize = 3;
/// Fixed header cells before the inline captured slots.
pub const AL_HDR_CELLS: usize = 4;
/// JIT/consumer-facing cell indices.
pub const ARENA_LAMBDA_FI_SLOT: usize = AL_FI_SLOT;
pub const ARENA_LAMBDA_CMP_SLOT: usize = AL_CMP_SLOT;

/// O(1), lock-free membership bitmap: which arena class-ids are lambda classes.
/// `is_arena_lambda` is consulted on the hot dispatch path for EVERY arena `Value::Obj`
/// receiver (even flag-OFF, once a consumer arm exists), so it must be a single atomic
/// load. Sized for up to `LAMBDA_CID_WORDS*64` arena classes (far above the few
/// thousand a boot registers); a class-id past the bitmap is simply "not a lambda".
const LAMBDA_CID_WORDS: usize = 4096; // 262144 class ids
/// Base of the direct-call GUARD space reserved for arena-lambda call sites.
///
/// A guard has to name the receiver population an entry was resolved for. The two
/// incumbent encodings are an interned class-name pointer (a real heap address) and an
/// arena class id + 1 (`< LAMBDA_CID_WORDS*64`, i.e. under 2^19). Neither works for a
/// capturing arena-lambda: its ADDRESS is fresh per instantiation, and its arena class
/// is shared by every site with the same capture KINDS. What is stable and per-site is
/// the interned site index, so it gets its own numeric window here.
///
/// Separation from the other two is a PERFORMANCE property, not the soundness argument.
/// Soundness comes from the per-call token re-check in `Vm::jit_direct_invoke`: an entry
/// reached by the wrong representation compares a site index against a `lambda_fp` FNV
/// fold (or vice versa), which cannot match, so the call degrades to the general path
/// rather than dispatching to the wrong target. A guard collision would therefore cost
/// a cache miss, never a mis-dispatch.
pub const AL_GUARD_BASE: usize = 1usize << 32;

#[cfg(test)]
mod tests;
mod gates;
pub(crate) use gates::*;
mod mapping;
pub(crate) use mapping::*;
mod classes;
pub(crate) use classes::*;
mod primitive_arrays;
pub(crate) use primitive_arrays::*;
mod reference_arrays;
pub(crate) use reference_arrays::*;
mod strings;
pub(crate) use strings::*;
pub(crate) mod lambdas;
pub(crate) use lambdas::*;
mod pinned;
pub(crate) use pinned::*;
pub(crate) mod tlab;
pub(crate) use tlab::*;
mod collect;
pub(crate) use collect::*;
mod census;
pub(crate) use census::*;
mod snapshot;
pub(crate) use snapshot::*;
mod genheap;
pub(crate) use genheap::*;
mod region;
pub(crate) use region::*;
mod shadow;
pub(crate) use shadow::*;
mod header;
pub(crate) use header::*;
