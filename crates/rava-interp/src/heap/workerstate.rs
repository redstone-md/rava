//! The worker JIT state, its guard, and the COW bloom constants.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::heap::*;

/// HANDLE EPOCH — the `Value::Ref`/`Value::Array` twin of `nextgen::gc_epoch`.
///
/// A handle is an INDEX into a slot table with a free list, so a swept (or eagerly
/// released) slot is re-issued to an unrelated object and a cache keyed by handle
/// silently aliases. Unlike the arena there is no move to observe, so the signal has
/// to be the RETIREMENT itself: every path that pushes a slot back onto a free list
/// bumps this. A consumer tags its entries with the value and drops them when it
/// changes. Bumped a handful of times per collection — never per object on a mutator
/// path — so an untagged build pays nothing.
pub(crate) static HANDLE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The call-site IDENTITY of a lambda: the impl method it targets. This tuple is a
/// CONSTANT of the `invokedynamic` instruction — every instantiation of one site shares
/// it — so it is interned to a single `Arc<LambdaSite>` and each closure holds only a
/// refcount bump. [[rava-heap-census-verdict]] measured 908545 lambdas costing 386.8MB
/// (30.4% of the live Rust heap): three fresh `String` allocs per instance for a value
/// that never varies. The three strings are themselves interned through the class-name
/// pool (`Heap::intern`), so `owner` (which usually EQUALS a loaded class name) is shared
/// with zero extra allocation. serde `rc` feature is enabled → `Arc<str>`/`Arc<LambdaSite>`
/// serialize for freeze/restore.
/// SERIALISATION GOES THROUGH THE THREE STRINGS ONLY. A derived `Deserialize` would
/// bypass `new()` and take `fp` from the wire — and restore data written before `fp`
/// existed has no such field at all, so the derive would need a `default` of 0: a
/// silently WRONG fingerprint on every restored site, in the one cache that exists to
/// avoid re-deriving it. Routing through `LambdaSiteWire` makes `fp` unforgeable — it can
/// only ever be the fold of the three strings, whatever the wire says.
#[derive(Clone, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(from = "LambdaSiteWire", into = "LambdaSiteWire")]
pub struct LambdaSite {
    pub owner: Arc<str>,
    pub name: Arc<str>,
    pub descriptor: Arc<str>,
    /// Fold of `owner`/`name`/`descriptor`, computed ONCE at construction. The
    /// direct-call cache re-checks a lambda receiver's content fingerprint on
    /// EVERY call (`Heap::lambda_fp`), and that fingerprint used to re-fold all
    /// three strings — ~100 bytes of FNV per dispatch for a value that is a
    /// constant of the `invokedynamic` instruction. A pure function of the three
    /// fields above, so the derived `Hash`/`PartialEq` stay consistent.
    pub fp: u64,
}

impl LambdaSite {
    /// The ONLY way to build a site: interns nothing (callers pass already-interned
    /// `Arc<str>`) but pins `fp` to the three strings, so no later reader has to
    /// re-derive it.
    pub fn new(owner: Arc<str>, name: Arc<str>, descriptor: Arc<str>) -> Self {
        let mut fp: u64 = 0x9E37_79B9_7F4A_7C15;
        for s in [&*owner, &*name, &*descriptor] {
            for &b in s.as_bytes() {
                fp = (fp.rotate_left(5) ^ b as u64).wrapping_mul(0x100_0000_01B3);
            }
        }
        Self { owner, name, descriptor, fp }
    }
}

/// Wire form of a `LambdaSite`: the three strings and nothing else. `fp` is derived on
/// the way in, so a restored site can never carry a fingerprint that disagrees with its
/// own strings, and data written before `fp` existed still loads.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct LambdaSiteWire {
    pub(crate) owner: Arc<str>,
    pub(crate) name: Arc<str>,
    pub(crate) descriptor: Arc<str>,
}

impl From<LambdaSiteWire> for LambdaSite {
    fn from(w: LambdaSiteWire) -> Self {
        LambdaSite::new(w.owner, w.name, w.descriptor)
    }
}

impl From<LambdaSite> for LambdaSiteWire {
    fn from(s: LambdaSite) -> Self {
        LambdaSiteWire { owner: s.owner, name: s.name, descriptor: s.descriptor }
    }
}

/// The block compiled code reads for native heap access. The JIT is handed its
/// ADDRESS (param 2); native loads read `desc_base` at offset 0, native stores
/// read `fault` at offset 8 (set after any array fault so a later store no-ops,
/// matching the interpreter's stop-on-throw). `#[repr(C)]` fixes those offsets.
/// Size of the global JIT field-slot hint table (power of two → bucket = fid &
/// (N-1)). 8192 u32 = 32 KB, fixed for the heap's life so its base never moves.
pub const JIT_FIELD_HINT_N: usize = 8192;

/// Number of getstatic value cells (one per compiled getstatic site). Each cell is
/// 2×i64 = 16 B, so 16384 cells = 256 KB, fixed for the heap's life (stable base).
pub const JIT_STATIC_SLOTS: usize = 16384;

pub(crate) const COW_BLOOM_FID_MUL: u64 = 0x9e37_79b1_85eb_ca87;
/// 16 KiB: small enough for private L1/L2 residency, large enough that the
/// at-most-dozens of concurrent element keys do not make unrelated workers
/// conservatively enter the shadow callback.
pub(crate) const COW_BLOOM_SLOTS: usize = 4096;
pub(crate) const COW_BLOOM_MASK: usize = COW_BLOOM_SLOTS - 1;
pub(crate) static COW_BLOOM_COUNTS: [std::sync::atomic::AtomicU32; COW_BLOOM_SLOTS] =
    [const { std::sync::atomic::AtomicU32::new(0) }; COW_BLOOM_SLOTS];

thread_local! {
    pub(crate) static WORKER_JIT_CTX: std::cell::Cell<*mut JitCtx> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
    pub(crate) static WORKER_ARRAY_CURSOR: std::cell::Cell<WorkerArrayCursor> =
        const { std::cell::Cell::new(WorkerArrayCursor::EMPTY) };
}

pub(crate) struct WorkerJitState {
    pub(crate) ctx: JitCtx,
    // Only the installed execution thread and its compiled frames may touch
    // this table. `UnsafeCell` makes that interior mutation explicit while the
    // owning guard keeps the allocation pinned for the whole compiled region.
    pub(crate) bloom: std::cell::UnsafeCell<[u32; COW_BLOOM_SLOTS]>,
}

/// Scoped worker view of otherwise immutable JIT descriptor bases plus an
/// exact thread-local COW summary. The second compiled ABI parameter already
/// carries this context through machine-code calls, so no method ABI changes.
pub(crate) struct WorkerJitCtxGuard {
    pub(crate) state: Box<WorkerJitState>,
    pub(crate) previous: *mut JitCtx,
}

impl WorkerJitCtxGuard {
    pub(crate) fn install(snapshot: JitCtx) -> Self {
        let mut state = Box::new(WorkerJitState {
            ctx: snapshot,
            bloom: std::cell::UnsafeCell::new([0; COW_BLOOM_SLOTS]),
        });
        state.ctx.cow_bloom_base = state.bloom.get().cast::<u32>() as usize;
        let current = &mut state.ctx as *mut JitCtx;
        let previous = WORKER_JIT_CTX.with(|slot| slot.replace(current));
        debug_assert!(previous.is_null(), "nested worker JIT context");
        Self { state, previous }
    }
}

impl Drop for WorkerJitCtxGuard {
    fn drop(&mut self) {
        // An abort may leave the current element's authoritative shadow live.
        // Balance its local counters before the backing table is released.
        crate::parallel::shadow_clear();
        WORKER_JIT_CTX.with(|slot| {
            debug_assert_eq!(slot.get(), &mut self.state.ctx as *mut JitCtx);
            slot.set(self.previous);
        });
    }
}

pub(crate) enum ActiveCowBloom {
    Shared(*const std::sync::atomic::AtomicU32),
    Worker(*mut u32),
}

// ═══════════════ M3 9.2d.3 alloc-generation (recycling-robust identity) ═══════════════
// A monotone counter stamped on each heap object/array at allocation. Unlike a handle (recycled via
// free_obj/free_arr), a generation NEVER repeats — so "gen < phase-snapshot" is a bulletproof "this
// object predates the phase" test, immune to the free-list artifact that inflated the 9.2d.1/.2 counts.
// Side tables (gated by the biome probe → zero production cost); keyed by handle, overwritten on reuse.
pub(crate) static ALLOC_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub(crate) static GEN_OBJ: std::sync::Mutex<Option<HashMap<u32, u64>>> = std::sync::Mutex::new(None);
pub(crate) static GEN_ARR: std::sync::Mutex<Option<HashMap<u32, u64>>> = std::sync::Mutex::new(None);
