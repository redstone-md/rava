//! The `extern "C"` bridges compiled code calls back into the host through.
//!
//! Sixty-nine of them lived interleaved with the codegen that emits their call sites. Grouped by
//! what they serve, each family is small enough to read on its own, and the direction of the
//! dependency is now visible: codegen emits a call, the bridge reconstructs `&mut Vm` and does
//! the work. Re-exported so no call site outside changed.
//!
//! ── THE PROLOGUE CENSUS: WHAT THE 77 BRIDGES PAY BEFORE ANY WORK ─────────────────────────
//!
//! A product boot takes ~30M compiled→host crossings, roughly four per compiled method entry,
//! and the crossing itself — not what sits on either side of it — is what this census prices.
//! Counted by reading every `extern "C" fn` in this directory, classified by which of the three
//! prologue statements it opens with (`cb(KIND)` the meter, a `vm_*()` lookup, and
//! `vm.jit_pending_is_set()` the region guard):
//!
//! | prologue shape                       | bridges | examples                                  |
//! |--------------------------------------|--------:|-------------------------------------------|
//! | meter + lookup + guard (all three)   |      32 | getfield, putfield, invoke, dispatch_miss  |
//! | lookup + guard, no meter             |       9 | deopt, throw, the four bitset bridges      |
//! | meter + lookup, no guard             |       6 | the four `String` reads, acmp, array_base  |
//! | lookup only                          |       6 | box_int, unbox_int, heap_objects_base      |
//! | meter only                           |       2 | f32_rem, f64_rem, arena_deopt_alloc        |
//! | NOTHING — already prologue-free      |      15 | card_mark, ic_base, 7 parallel bridges + JIT poll |
//!
//! So 53 of 77 bridges take the lookup and 41 of them also take the guard. The prologue is the
//! single most repeated sequence in the engine, which is why it is worth pricing by shape rather
//! than by family.
//!
//! **WHAT EACH OF THE THREE COSTS, in the units `crate::tls_cluster` established** — a
//! `thread_local!` lowers to a bare TLS-slot load ONLY when it is `const`-initialised AND its
//! type has no destructor; anything else compiles to an out-of-line `LocalKey::with` carrying a
//! lazy-init / destructor-registration state check on EVERY access:
//!
//! 1. **The lookup** (`jit/vmref.rs`) — `CURRENT_VM` is `Cell<*mut Vm>`, `const`-initialised,
//!    destructor-free: already the cheap shape, a TLS-slot load and a null test. What it was
//!    actually paying was the accessor's own out-of-line `call`, because none of the four
//!    accessors carried `#[inline]` while being consumed from twelve other files. FIXED: one
//!    `#[inline(always)]` primitive `vm_ptr()` that all four now wrap, and the gil-free denial
//!    arm of `vm_ref()` split into a `#[cold]` function so the serial path is the `gilfree()`
//!    test falling straight through.
//! 2. **The guard** — `Vm::jit_pending_is_set` reaches `execution_state::STATE`, whose payload
//!    owns an `Option<Value>` and therefore HAS a destructor. That is the expensive TLS shape,
//!    and 41 bridges paid it on every crossing. FIXED, in `execution_state.rs`, exactly as this
//!    census predicted: **the BIT moved, the VALUE did not.** A second `thread_local!`
//!    (`PENDING_BITS`, a `const`-initialised destructor-free `Cell<u8>`) mirrors
//!    `pending.is_some()`, so the guard is now a bare TLS-slot load and two bit tests; the
//!    `Option<Value>` stays at the address `PENDING_ROOTS` published, so the STW collector's
//!    contract is untouched. The mirror is maintained by the five writers in that file under
//!    one rule — **the flag is never ahead of the value** (raised only after a store, lowered
//!    only before a take) — so an observer catching an intermediate state reads the older of
//!    two consistent states, never a raised flag over an empty slot. Kill switch
//!    `RAVA_NO_FAST_PENDING=1` (and the block switch `RAVA_NO_EMIT_MORE=1`) sends every read
//!    back through `LocalKey::with` on the value-owning slot.
//! 3. **The meter** (`jit/counters.rs`) — `cb()` is three gates deep, and only the first two are
//!    gate-ordered: `callback_stats_enabled()` short-circuits and `s09census::note` tests its
//!    `OnceLock` before touching TLS, but `gilfree_cf::note_cb(i)` in between is an UNGATED
//!    thread-local read-modify-write on all 30M crossings, whose consumers (`last_cb`, `cb_n`)
//!    are read only when a gil-free lane aborts. NOT FIXED, also not in this directory.
//!
//! With 1 and 2 fixed, the guard and the lookup are both bare TLS loads; the meter (3) is the
//! one remaining out-of-line cost in the prologue.
//!
//! **THE ONE-LINE ADOPTION, when 3 is ready.** The 32 bridges with the full shape open
//! with three statements that fold into one call — `let Some(vm) = cb_enter(KIND) else { return
//! 0 };` — which is what puts the meter, the lookup and the guard behind a single gate ordering
//! and a single inlining decision. It is deliberately NOT written yet: folding them today would
//! move three costs into one function without removing any of them, and this tree has measured
//! twice that a memo over a path that is already cheap loses.
//!
//! **WHICH BRIDGES NEED NO PROLOGUE AT ALL ONCE THE CONTEXT IS A PINNED REGISTER.** `vm_ptr()`
//! is the only place a `Vm` address enters host code, so pinning turns it from a TLS read into a
//! register read. Six bridges have the lookup as their ENTIRE prologue and become literally
//! prologue-free that day: `rava_box_int`, `rava_unbox_int`, `rava_heap_objects_base`,
//! `rava_heap_pic_shadow`, `rava_ring_store_guard`, `rava_aload_verify`. Fifteen more already
//! are (`rava_cells_push_slow`, `rava_box_exit`, `rava_card_mark`, `rava_bare_verify`,
//! `rava_jit_ctx_addr`, `rava_jit_cursor_addr`, `rava_ic_base`, seven `rava_parallel_*`, and
//! `rava_jit_safepoint`).
//! The six `meter + lookup` bridges — the four `String` reads, `rava_acmp`, `rava_array_base_len`
//! — drop to the meter alone.

pub(crate) mod deopt;
mod fields;
pub(crate) use fields::*;
#[cfg(feature = "next-gen-gc")]
mod field_repr;
#[cfg(feature = "next-gen-gc")]
pub(crate) use field_repr::*;
mod strings;
pub(crate) use strings::*;
mod bitset;
pub(crate) use bitset::*;
mod parallel;
pub(crate) use parallel::*;
mod dispatch;
pub(crate) use dispatch::*;
mod invoke;
pub(crate) use invoke::*;
mod arrays;
pub(crate) use arrays::*;
mod primitive_arrays;
pub(crate) use primitive_arrays::*;
mod maps;
pub(crate) use maps::*;
mod alloc;
pub(crate) use alloc::*;
mod checkcast;
pub(crate) use checkcast::*;
mod npe_site;
pub(crate) use npe_site::*;
mod arith;
pub(crate) use arith::*;
mod bases;
pub(crate) use bases::*;

/// Compiled getstatic sites that had to leave for the host to refill their value cell. A cell
/// misses when it is cold — or when ANY `putstatic` anywhere bumped the global `static_gen`,
/// which invalidates every cell in the process at once.
pub(crate) static GETSTATIC_REFILLS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
