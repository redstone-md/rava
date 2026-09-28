//! The per-GC-epoch arena value-object KEY MEMO, as one owner.
//!
//! ⭐**WHAT THIS TOOK OUT OF THE PROFILE.** Phase A profiled alone puts
//! `LocalKey::with<RefCell<(u64, HashMap<u64, Box<str>>)>> ... map::valkey_memo_with` at **1.52%**
//! of live samples — the second-heaviest host row under the block-state build, above
//! `Vm::jit_call_body`. The memo itself is right: the hit rate on the combo fold is 99.99% and a
//! hit replaces a guest `toString()` measured at ~950 ns. What the row is charging for is the
//! ENVELOPE around the lookup, and every part of that envelope was avoidable:
//!
//! | was (`crate::map::VALKEY_MEMO`)                     | is (here)                              |
//! |-----------------------------------------------------|----------------------------------------|
//! | `thread_local!` with a NON-const initialiser         | `const {}` initialiser                 |
//! | `RefCell` — flag load, range check, +1 store, guard drop −1 store | `UnsafeCell`, no flag    |
//! | value `Box<str>` = 16 B in-bucket **+ one `mi_malloc` block per put** | `Span{off,len}` = 8 B, **zero mallocs** |
//! | key bytes at a random malloc address (3rd dependent miss on a hit) | key bytes contiguous in one buffer |
//!
//! **Table byte size.** A `HashMap<u64, Box<str>>` bucket is 8 (key) + 16 (ptr+len) = **24 B**,
//! plus hashbrown's 1 control byte = **25 B per slot**; a `HashMap<u64, Span>` bucket is 8 + 8 =
//! **16 B**, **17 B per slot** — a **32% smaller table for the same population**, and hashbrown
//! sizes to the next power of two at ≤ 7/8 load, so e.g. 4,096 live entries occupy 8,192 slots =
//! **136 KB here against 200 KB before**, and the before-figure additionally chases 4,096
//! separate heap blocks. The string bytes move into ONE `Vec<u8>` that is `clear()`ed (capacity
//! retained) on an epoch flip, so the steady state does no allocation at all.
//!
//! ⚠**This is deliberately NOT a bigger cache.** The tree has measured the opposite change:
//! growing an L1 slot table from 2.5 KB to 14 KB was **+6.8% WORSE**. Nothing here adds a slot,
//! a mirror or a side table — the population, the admission rule, the epoch tag and the (absent)
//! capacity bound are byte-for-byte the ones `crate::map` already used. Only the ENCODING of the
//! value and the ENVELOPE around the probe changed.
//!
//! **TWO POPULATIONS, ONE TABLE TYPE.** `MEMO` is the per-GC-epoch VALUE-object key memo described
//! above. `RENDER` is the arena RENDER memo that used to live in `keys/mod.rs` as
//! `ARENA_KEYRENDER_MEMO: RefCell<(u64, FastMap<u32, Box<str>>)>` — the identical was-column, one
//! level up the funnel — moved onto this shape by `render_get`/`render_put` under kill switch
//! `RAVA_NO_FAST_MAPKEYTLS=1`. They are separate `thread_local`s with separate tag spaces; sharing
//! the STRUCT is what stops the two from drifting on what a span means.
//!
//! **Kill switch `RAVA_NO_FAST_VALKEY=1`** (default ON, also off under
//! `crate::flags::emit_more_disabled()`): every entry point below then forwards to the original
//! `crate::map::valkey_memo_{get,put,with}`, which are left exactly as they were and are the COLD
//! ARM. The two tables are independent and neither is written while the other serves, so even a
//! hypothetical mix could only ever MISS — and a miss is a recompute, i.e. the same answer.

/// DEFAULT ON. Off → the original `RefCell`/`Box<str>` memo in `crate::map` serves, unchanged.
#[cfg(feature = "next-gen-gc")]
fn fast_valkey() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_VALKEY").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Where one memoised key's bytes live inside [`Memo::buf`]. 8 bytes, `Copy`, no drop glue —
/// which is the whole point: it replaces a `Box<str>` that was 16 bytes IN the bucket and owned
/// a separate heap block besides.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
struct Span {
    off: u32,
    len: u32,
}

/// Above this, an epoch flip DROPS the byte buffer instead of retaining its capacity.
///
/// Retaining capacity is what makes a put allocation-free in the steady state, but the memo has
/// no entry cap (neither did the original — see `state left alone` in the module report), and a
/// single worldgen render can be hundreds of KB. 1 MiB per thread is the ceiling on what that
/// convenience may hold onto across a collection.
#[cfg(feature = "next-gen-gc")]
const BUF_RETAIN_MAX: usize = 1 << 20;

#[cfg(feature = "next-gen-gc")]
struct Memo {
    /// GC epoch this table describes. `crate::nextgen::gc_epoch()` bumps on every collection;
    /// entries from a prior epoch MISS, because a collection may hand a live arena address to a
    /// different object. Identical rule to the memo this replaces.
    epoch: u64,
    /// arena address → span of `buf`.
    tab: crate::FastMap<u64, Span>,
    /// Every memoised key's UTF-8 bytes, appended in put order. Cleared with `tab`, never
    /// individually — so a `Span` recorded under the current `epoch` is always in range and
    /// always the bytes that were appended for it.
    buf: Vec<u8>,
}

#[cfg(feature = "next-gen-gc")]
impl Memo {
    /// `const` so the `thread_local!` below can use a `const {}` initialiser: that removes the
    /// lazy-initialisation branch and the initialiser closure from every access, leaving only the
    /// destructor-registration state byte. `HashMap::with_hasher` and `BuildHasherDefault::new`
    /// are both const since 1.85; the empty map and the empty `Vec` allocate nothing.
    const fn new() -> Self {
        Memo {
            epoch: 0,
            tab: std::collections::HashMap::with_hasher(std::hash::BuildHasherDefault::new()),
            buf: Vec::new(),
        }
    }

    /// Start a fresh epoch: table and buffer are cleared TOGETHER, so no `Span` can outlive the
    /// bytes it points at.
    #[inline]
    fn reset(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.tab.clear();
        if self.buf.capacity() > BUF_RETAIN_MAX {
            self.buf = Vec::new();
        } else {
            self.buf.clear();
        }
    }

    /// The stored bytes for arena address `addr`, or `None` (wrong epoch, or absent).
    #[inline]
    fn lookup(&self, addr: u64, epoch: u64) -> Option<&str> {
        if self.epoch != epoch {
            return None;
        }
        let s = *self.tab.get(&addr)?;
        let bytes = &self.buf[s.off as usize..s.off as usize + s.len as usize];
        // SAFETY: `insert` only ever appends `&str` bytes and records the span of that exact
        // append; spans never straddle two appends and the buffer is only ever cleared together
        // with the table (`reset`). So these bytes are a whole, unmodified `&str`.
        Some(unsafe { std::str::from_utf8_unchecked(bytes) })
    }

    /// Record `key` for `addr` under `epoch`.
    ///
    /// `cap` bounds the ENTRY count: at or above it the table and its byte buffer are cleared
    /// wholesale. That is the bound the arena RENDER memo carried inline as `KEYRENDER_MAX`
    /// (65,536 entries); the value memo passes `usize::MAX`, which is the absence of a bound it
    /// always had. Clearing wholesale keeps the common case allocation-free — an eviction policy
    /// would cost more than the render it protects, and a span cannot be freed individually
    /// anyway.
    #[inline]
    fn insert(&mut self, addr: u64, key: &str, epoch: u64, cap: usize) {
        if self.epoch != epoch {
            self.reset(epoch);
        } else if let Some(&s) = self.tab.get(&addr) {
            // A re-put of the SAME bytes at the SAME address is a no-op. The `Box<str>` encoding
            // this replaces FREED the old block on overwrite; an append-only buffer would instead
            // ORPHAN the previous span until the next reset, so an identical re-put must not
            // append. (Both call sites put only after a miss, so this is a belt-and-braces bound
            // on `buf` growth, not the common path.)
            if self.buf[s.off as usize..s.off as usize + s.len as usize] == *key.as_bytes() {
                return;
            }
        }
        if self.tab.len() >= cap {
            self.reset(epoch);
        }
        let off = self.buf.len();
        // The offset is a `u32`. Nothing observed comes near 4 GiB of key text in one epoch, but
        // an overflow would alias two keys, so it is a hard REFUSAL, not a wrap: skipping the put
        // costs a later recompute, which is the same answer.
        if off + key.len() > u32::MAX as usize {
            return;
        }
        self.buf.extend_from_slice(key.as_bytes());
        self.tab
            .insert(addr, Span { off: off as u32, len: key.len() as u32 });
    }
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    static MEMO: std::cell::UnsafeCell<Memo> = const { std::cell::UnsafeCell::new(Memo::new()) };
}

/// ⭐**THE ARENA RENDER MEMO, MOVED ONTO THE SAME TABLE SHAPE.**
///
/// `crate::map::keys::ARENA_KEYRENDER_MEMO` (still there, now the kill-switch arm) is
/// `RefCell<(u64, FastMap<u32, Box<str>>)>` with a NON-const initialiser — the exact shape this
/// file already took apart one level down the key funnel:
///   * non-const init ⇒ every touch is an out-of-line `LocalKey::with` carrying a
///     lazy-initialisation state check and the initialiser closure;
///   * `RefCell` ⇒ borrow-flag load, compare, +1 store on entry, release store on the guard drop;
///   * `Box<str>` value ⇒ **one allocator block per put**, and on a hit a third dependent cache
///     miss to reach bytes that are then copied AGAIN into the `String` the caller returns.
///
/// **TABLE BYTE SIZE — it SHRINKS.** Was `(u32, Box<str>)` = 4 + 16 padded to **24 B**, +1
/// hashbrown control byte = **25 B/slot**, plus one separate heap block per entry. Is
/// `(u64, Span)` = 8 + 8 = **16 B**, **17 B/slot**, **zero per-entry blocks**; the key text moves
/// into one `Vec<u8>` that is cleared (capacity retained, dropped above 1 MiB) on a tag flip. Same
/// population, same `KEYRENDER_MAX` = 65,536 entry bound, same invalidation tag. Nothing is added
/// beside it — the tree's +6.8% lesson (an L1 slot table grown 2.5 KB → 14 KB) is respected in the
/// only direction that matters.
///
/// The `epoch` field carries the render memo's `gc_epoch ^ static_gen.rotate_left(32)` TAG rather
/// than a bare epoch. It is only ever compared for equality, never interpreted, so the two uses of
/// the same struct do not interfere — and the two tables are separate `thread_local`s besides.
#[cfg(feature = "next-gen-gc")]
thread_local! {
    static RENDER: std::cell::UnsafeCell<Memo> = const { std::cell::UnsafeCell::new(Memo::new()) };
}

/// The ONLY place a `&mut Memo` is produced.
///
/// SAFETY / why the `RefCell` could go: the borrow flag existed to catch a nested borrow, and the
/// five entry points below are the complete set of callers. None of them runs guest code, calls
/// the VM, or invokes a caller-supplied closure while the reference is live — `fold` calls only
/// `super::combo_fnv` (pure, over a byte slice), `get`/`render_get` allocate a `String`,
/// `put`/`render_put` may grow two buffers. The global allocator cannot re-enter this module, so
/// no second borrow can exist while one is outstanding, and the `&mut` never escapes `f`. The memo
/// is thread-local and immortal, so there is no cross-thread aliasing either. `f` is private to
/// this file and must stay that way.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn with_memo<R>(
    k: &'static std::thread::LocalKey<std::cell::UnsafeCell<Memo>>,
    f: impl FnOnce(&mut Memo) -> R,
) -> R {
    k.with(|c| f(unsafe { &mut *c.get() }))
}

/// ⭐**THE HOT ENTRY: fold a memoised key's bytes into `ph` and return their length.**
///
/// This replaced `crate::map::valkey_memo_with(p, epoch, |s| { ..fnv.. })` at the two combo-fold
/// call sites. Two things went with the closure. First, a caller-supplied `FnOnce` is exactly the
/// re-entrancy the `RefCell` was guarding against, so making the operation CONCRETE is what let
/// the borrow flag go. Second, both call sites had written out the same FNV-1a loop; folding here
/// through `super::combo_fnv` leaves ONE writer of those bytes — and a differing byte would be a
/// differing `MapKey::Hash`, i.e. a silent miss, which is why that matters.
///
/// Bit-exact: same stored key, same bytes, same seed, same multiply order.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn fold(p: crate::arena::ArenaPtr, epoch: u64, ph: &mut u64) -> Option<usize> {
    if !fast_valkey() {
        return crate::map::valkey_memo_with(p, epoch, |s| {
            *ph = super::combo_fnv(s.as_bytes(), *ph);
            s.len()
        });
    }
    let addr = p.addr();
    with_memo(&MEMO, |m| {
        let s = m.lookup(addr, epoch)?;
        *ph = super::combo_fnv(s.as_bytes(), *ph);
        Some(s.len())
    })
}

/// The owned-`String` entry, for the render path that must RETURN the key.
///
/// The allocation here is the caller's requirement (`key_str_mut_in` returns `String`), not the
/// memo's: what this version removes is the OTHER copy — the one `put` used to make into a
/// `Box<str>`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn get(p: crate::arena::ArenaPtr, epoch: u64) -> Option<String> {
    if !fast_valkey() {
        return crate::map::valkey_memo_get(p, epoch);
    }
    let addr = p.addr();
    with_memo(&MEMO, |m| m.lookup(addr, epoch).map(str::to_owned))
}

/// Record `key` for `p` under `epoch`.
///
/// The caller's admission rule is unchanged and still lives at the call site: it only puts when no
/// collection ran across the (allocating) guest `toString()`, because a collection would have
/// moved `p` and made `p.addr()` a stale alias.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn put(p: crate::arena::ArenaPtr, key: &str, epoch: u64) {
    if !fast_valkey() {
        crate::map::valkey_memo_put(p, key, epoch);
        return;
    }
    let addr = p.addr();
    // `usize::MAX` = the (absent) entry bound this memo always had; see `Memo::insert`.
    with_memo(&MEMO, |m| m.insert(addr, key, epoch, usize::MAX));
}

/// Read the arena RENDER memo: the key text recorded for identity stamp `id` under invalidation
/// tag `tag`, or `None` (wrong tag, or absent).
///
/// The `String` is the CALLER's requirement (`arena_funnel_get`/`arena_key_memoed` both return
/// `String`), not the memo's — what the span encoding removes is the OTHER copy, the `Box<str>`
/// the put used to allocate.
#[cfg(feature = "next-gen-gc")]
pub(super) fn render_get(id: u32, tag: u64) -> Option<String> {
    with_memo(&RENDER, |m| m.lookup(id as u64, tag).map(str::to_owned))
}

/// Record `key` for identity stamp `id` under invalidation tag `tag`, bounded at `cap` entries.
///
/// The admission rule (identity stamp non-zero, every instance field `final` up the chain) is
/// unchanged and still lives at the call site, exactly as the value memo's does.
#[cfg(feature = "next-gen-gc")]
pub(super) fn render_put(id: u32, key: &str, tag: u64, cap: usize) {
    with_memo(&RENDER, |m| m.insert(id as u64, key, tag, cap));
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod tests {
    use super::Memo;

    /// No entry bound — what the value memo passes.
    const UNCAPPED: usize = usize::MAX;

    /// The span encoding must survive interleaved puts, an epoch flip, and a re-put at the same
    /// address — the three ways a `Span` could be made to point at the wrong bytes.
    #[test]
    fn spans_do_not_alias_across_puts_or_epochs() {
        let mut m = Memo::new();
        let (a, b) = (0x1000u64, 0x2000u64);
        m.insert(a, "o:Direction{name=north}", 7, UNCAPPED);
        m.insert(b, "o:x", 7, UNCAPPED);
        assert_eq!(m.lookup(a, 7), Some("o:Direction{name=north}"));
        assert_eq!(m.lookup(b, 7), Some("o:x"));
        // Wrong epoch never serves.
        assert_eq!(m.lookup(a, 8), None);
        // An epoch flip clears both halves; the same address now holds a different object.
        m.insert(a, "o:Other", 8, UNCAPPED);
        assert_eq!(m.lookup(a, 8), Some("o:Other"));
        assert_eq!(m.lookup(b, 8), None);
    }

    /// An identical re-put must not append (it would orphan the previous span); a DIFFERENT
    /// value at the same address must still take effect.
    #[test]
    fn identical_reput_does_not_grow_the_buffer() {
        let mut m = Memo::new();
        let a = 0x1000u64;
        m.insert(a, "o:same", 3, UNCAPPED);
        let after_first = m.buf.len();
        m.insert(a, "o:same", 3, UNCAPPED);
        assert_eq!(m.buf.len(), after_first, "identical re-put appended");
        assert_eq!(m.lookup(a, 3), Some("o:same"));
        m.insert(a, "o:different", 3, UNCAPPED);
        assert_eq!(m.lookup(a, 3), Some("o:different"));
    }

    /// The entry bound clears table and buffer TOGETHER, so no surviving span can dangle.
    #[test]
    fn the_entry_cap_clears_both_halves() {
        let mut m = Memo::new();
        m.insert(1, "o:a", 5, 2);
        m.insert(2, "o:b", 5, 2);
        assert_eq!(m.lookup(1, 5), Some("o:a"));
        // Third put is at the cap: everything is dropped, then the new entry lands.
        m.insert(3, "o:c", 5, 2);
        assert_eq!(m.lookup(1, 5), None);
        assert_eq!(m.lookup(2, 5), None);
        assert_eq!(m.lookup(3, 5), Some("o:c"));
        assert_eq!(m.buf.len(), 3);
    }

    /// A fold over a memo hit must produce exactly what folding the same bytes produces.
    #[test]
    fn fold_matches_a_direct_fnv_over_the_stored_bytes() {
        let mut m = Memo::new();
        let a = 0x3000u64;
        m.insert(a, "o:BlockState{minecraft:stone}", 1, UNCAPPED);
        let mut ph = 0xcbf2_9ce4_8422_2325u64;
        let s = m.lookup(a, 1).unwrap();
        let len = {
            ph = super::super::combo_fnv(s.as_bytes(), ph);
            s.len()
        };
        let mut want = 0xcbf2_9ce4_8422_2325u64;
        want = super::super::combo_fnv(b"o:BlockState{minecraft:stone}", want);
        assert_eq!(ph, want);
        assert_eq!(len, 29);
    }
}
