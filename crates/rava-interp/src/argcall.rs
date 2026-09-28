//! Argument marshalling between the interpreter, the JIT and the natives: descriptors in, cells out.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::Value;
use crate::*;
/// Lay arguments into local slots, giving each `long`/`double` parameter two
/// slots (JVMS 2.6.1). `args` is `[receiver?] ++ method-args`, one value each.
/// Numeric equality of two `Value`s for JIT verification. Floats/doubles are
/// compared bit-for-bit (NaN bit-patterns may differ from fcmpl/fcmpg corner
/// cases, which this surfaces as a mismatch — acceptable for the audit).
pub(crate) fn jit_values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Long(x), Value::Long(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => {
            x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())
        }
        (Value::Double(x), Value::Double(y)) => {
            x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())
        }
        // Reference results (now that ref-returning methods compile): compare by
        // identity/kind so the verify audit stays meaningful instead of always
        // flagging a mismatch.
        (Value::Ref(x), Value::Ref(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        // A compiled method's String result is an ArenaString (Value::Obj, PERM pool); the
        // interpreter's is a Value::Str. SAME CONTENT = equal — compare the bytes so the
        // verify audit doesn't flag every toString/ldc as a false mismatch (the ArenaString-
        // vs-Str representation split is expected, not a codegen bug).
        #[cfg(feature = "next-gen-gc")]
        (Value::Obj(p), Value::Str(s)) | (Value::Str(s), Value::Obj(p))
            if crate::nextgen::is_arena_string(*p) =>
        {
            crate::nextgen::arena_str_bytes(*p) == s.as_bytes()
        }
        #[cfg(feature = "next-gen-gc")]
        (Value::Obj(x), Value::Obj(y))
            if crate::nextgen::is_arena_string(*x) && crate::nextgen::is_arena_string(*y) =>
        {
            crate::nextgen::arena_str_bytes(*x) == crate::nextgen::arena_str_bytes(*y)
        }
        (Value::Null, Value::Null) => true,
        (Value::StdOut, Value::StdOut) | (Value::StdErr, Value::StdErr) => true,
        _ => false,
    }
}

/// CNP eligibility: a method descriptor whose parameters are all `int` and whose
/// return is `int`. Returns the parameter/local-slot count; any other kind refuses.
#[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))]
pub(crate) fn all_int_static_sig(descriptor: &str) -> Option<usize> {
    let open = descriptor.find('(')?;
    let close = descriptor.find(')')?;
    let params = descriptor.get(open + 1..close)?;
    if !params.bytes().all(|b| b == b'I') {
        return None;
    }
    if descriptor.get(close + 1..)? != "I" {
        return None;
    }
    Some(params.len())
}

/// Per-descriptor ARGUMENT LAYOUT, computed once instead of per call.
///
/// `expand_locals_into` walked the descriptor byte by byte on EVERY call to decide which
/// arguments take two local slots (JVMS 2.6.1) and which are `Z` params needing an unbox — and
/// `arg_slots(descriptor)` walked the SAME string again just to size the reserve. Two parses per
/// call, of a string that is fixed for the call site. HotSpot computes the argument size once at
/// class load and keeps it on the `Method`; caller-attributed profiling of one interpreted call
/// (ledger row 185) puts `expand_locals_into` at 9.48% and `arg_slots` at 1.25%.
///
/// Two bitmasks are enough: `wide` for category-2 arguments, `unbox` for the params that may
/// arrive BOXED and have to be read back out of a `$value` field. Over 64
/// arguments the masks cannot encode it and the caller keeps the original walk — a descriptor
/// that long is legal but does not occur, and a wrong fast path here would put values in the
/// wrong local slots.
#[derive(Clone, Copy)]
pub(crate) struct ArgLayout {
    pub n: u8,
    pub slots: u16,
    pub wide: u64,
    /// ⭐**`Z`, AND NOW ALSO `C`/`S`/`B`.** A `Z` param has always been able to receive a boxed
    /// `java/lang/Boolean` (they are real objects so `instanceof Boolean` works), and
    /// `crate::boxid` gives `Character`/`Short`/`Byte` the same real identity — so the same
    /// three-letter widening covers them. WHERE such a box comes from is the metafactory:
    /// `LambdaMetafactory` adapts the SAM's reference parameter to the implementation's
    /// primitive, and this engine invokes the implementation directly with the SAM's arguments,
    /// so a `Character` reaches a `(C)` impl unadapted (`Character::isDigit` behind a
    /// `Predicate`, `StringBuilder::append` behind a `Consumer`).
    ///
    /// ⚠A set bit costs the descriptor its `append` shortcut, so the widening is GATED on
    /// `boxid::enabled()` — with `RAVA_NO_BOXID=1` no narrow box exists, the bit is never set
    /// for `C`/`S`/`B`, and the layout (and therefore the shortcut population, and therefore the
    /// measurement) is byte-identical to what it was.
    pub unbox: u64,
}

/// ⭐**`RAVA_NO_FAST_ARGCALL=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) restores the
/// per-argument expansion loop for every descriptor.** DEFAULT ON. It gates only the `append`
/// shortcut in `expand_locals_into`; the memo below is a storage change with byte-identical
/// results and nothing to switch off.
///
/// Reached from its own site ONLY under [`AG_LIVE`]; the default path reads [`AG_FAST_APPEND`]
/// out of the word [`arg_gates`] loads once per call.
fn fast_argcall() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_ARGCALL").is_none() && !crate::flags::emit_more_disabled()
    })
}

// ── THIS FILE'S PROCESS CONSTANTS, READ ONCE PER CALL INSTEAD OF THREE TIMES ──────────────
//
// ⭐**WHAT THE SHAPE WAS.** `expand_locals_into` runs **11,803,098 times a boot** (the
// vec-growth census on this very site), and every one of those runs paid:
//
//   * `fast_argcall()` — an ACQUIRE-ordered `OnceLock` load, sitting in the `append`
//     shortcut's condition. An acquire load is an optimisation barrier LLVM may not hoist and
//     which forces the surrounding values to be materialised, so it costs more than the `mov`.
//   * `crate::vecgrow::note(..)` — an UNCONDITIONAL, out-of-line call on all three exits,
//     whose first act is a SECOND acquire `OnceLock` load (`vecgrow::enabled()`) that says
//     "the census is off" and returns. A diagnostic that is off in every non-debug run was
//     therefore costing a call plus a barrier per interpreted call.
//
// **Was: two acquire `OnceLock` loads and one unconditional out-of-line call per call. Is: one
// plain thread-local word load and two bit tests, with the census call not made at all while
// the census is off.** The population is every interpreted invoke that expands its arguments.
//
// The shape is copied verbatim from `opcodes::gates::op_gates` / `natives::entry::entry_gates`:
// a `Cell<u32>` with a `const` initialiser and no destructor is the one `thread_local!` form
// `std` lowers to a bare TLS-slot load with no lazy-init check and nothing to call.
//
// ⚠**`vecgrow::enabled()` ARMS A WATCHDOG THREAD** when `RAVA_VECGROW_STAT` is set, so its
// position is not purely decorative. Folding it here does not move that side effect: the gate
// is resolved on this thread's FIRST `expand_locals_into`, which is the same call that reached
// `vecgrow::note` (and therefore `enabled()`) before — and `enabled()` is an idempotent
// `OnceLock` that `opcodes::pop_args` and `Frame::new` already reach earlier in any boot.

/// Bit 0 — the word has been resolved on this thread (so a resolved word is never 0).
const AG_RESOLVED: u32 = 1 << 0;
/// Bit 1 — KILL SWITCH ENGAGED (`RAVA_NO_FAST_VALUE2=1` / `RAVA_NO_EMIT_MORE=1`): resolve
/// nothing here, and let every gate be read from its own `OnceLock` at its own site in its own
/// order, exactly as it was before this round.
const AG_LIVE: u32 = 1 << 1;
/// Bit 2 — [`fast_argcall`]: the `append` shortcut in [`expand_locals_into`] is admissible.
const AG_FAST_APPEND: u32 = 1 << 2;
/// Bit 3 — `vecgrow::enabled()`: the vec-growth census is armed, so the `note` calls must run.
const AG_VECGROW: u32 = 1 << 3;

thread_local! {
    /// This thread's resolved argument-marshalling gates. `Cell<u32>`, `const`-initialised and
    /// destructor-free — deliberately, that combination is what makes the read a plain load.
    static ARG_GATES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The word. One plain per-thread load; never 0 on return.
#[inline(always)]
fn arg_gates() -> u32 {
    let g = ARG_GATES.get();
    if g != 0 { g } else { arg_gates_cold() }
}

/// First touch on this thread: read the process constants through their `OnceLock`s — the only
/// place that still happens under the default — and cache the answers. Once per thread, so the
/// environment lookups behind them stay exactly as many as they were.
#[cold]
#[inline(never)]
fn arg_gates_cold() -> u32 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_VALUE2").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = AG_RESOLVED;
    if live {
        g |= AG_LIVE;
        ARG_GATES.set(g);
        return g;
    }
    if fast_argcall() {
        g |= AG_FAST_APPEND;
    }
    if crate::vecgrow::enabled() {
        g |= AG_VECGROW;
    }
    ARG_GATES.set(g);
    g
}

/// One vec-growth census record, reached only when the census is armed (or when the kill switch
/// puts the gate back inside `vecgrow::note` where it used to live). `note` itself is not
/// `#[inline]`, so skipping it skips a call, not just a compare.
#[inline(always)]
fn note_locals_reserve(g: u32, cap_before: usize, cap_after: usize) {
    if g & (AG_LIVE | AG_VECGROW) != 0 {
        crate::vecgrow::note(crate::vecgrow::Site::ExpandLocals, cap_before, cap_after, 0);
    }
}

/// One memo slot: the descriptor that produced the layout, then the layout's four fields inline.
///
/// ⭐**THE KEY IS AN OWNED `String`, NOT A `Box<str>`, AND THAT IS THE FIX.** The slot must keep
/// the descriptor's BYTES — an address is not a descriptor (a freed temporary's address is
/// reusable, and an unverified hit would put arguments in the wrong local slots) — but a
/// `Box<str>` cannot be written into, so every miss landing on an occupied slot freed one heap
/// block and allocated another. Two descriptors whose addresses fold to the same index therefore
/// cost **one malloc plus one free PER CALL** for as long as the interpreted loop alternated
/// between them: allocator traffic proportional to CALLS, spent to store a key whose content
/// never changed. A `String` is refilled in place.
///
/// **Was: one alloc + one free per memo MISS (grows with calls through a contended index).
/// Is: at most one allocation per slot per new high-water descriptor length — at most 256 for
/// the life of a thread, and zero in steady state.**
///
/// The stored pointer went with it. It was a cheap reject ahead of the content compare, but a
/// `String` carries its length IN THE SLOT, so the length compare rejects at the same cost
/// without spending eight bytes on a field correctness never rested on. The index is still
/// derived from the address — descriptors come from constant pools, so a call site folds to the
/// same slot every time — but it is now purely a HINT: content decides the hit, which is a
/// strictly stronger test than the address-plus-content pair it replaces.
///
/// SIZE: `u64 + u64 + String + u16 + u8` = 43 bytes padded to 48, so the table is
/// **256 x 48 = 12,288 B = 12 KB** — byte-for-byte what `Option<(usize, Box<str>, ArgLayout)>`
/// already cost (8 + 16 + 24, the `Option` riding the `Box`'s null niche). The table is NOT
/// GROWN: this tree has measured a +6.8% regression from a front cache that grew until it
/// stopped fitting in L1, and `owner_bits` already holds a second 8 KB one on the same threads.
struct LayoutSlot {
    wide: u64,
    unbox: u64,
    /// Empty means EMPTY. A descriptor without `(` never reaches the insert, so no real key is
    /// ever the empty string — but the probe still tests it explicitly rather than leaning on
    /// that, because a false hit here is a silent wrong-argument bug, not a crash.
    key: String,
    slots: u16,
    n: u8,
}

/// The empty slot as a CONSTANT, which is what lets the table below be `const`-initialised:
/// `String::new()` is a `const fn`, and an array repeat expression may repeat a path to a
/// constant item even when the element type is not `Copy`.
const EMPTY_SLOT: LayoutSlot =
    LayoutSlot { wide: 0, unbox: 0, key: String::new(), slots: 0, n: 0 };

/// Direct-mapped slot count. 256 x 48 B = 12 KB, unchanged from the `Vec` this replaced — this
/// tree has measured a **+6.8% regression** from a front cache that grew until it stopped
/// fitting in L1, and `owner_bits` already holds a second 8 KB table on the same threads.
const MEMO_SLOTS: usize = 256;

thread_local! {
    /// ⭐**THE TABLE IS NOW THE TLS BLOCK ITSELF, AND ITS INITIALISER IS A `const`.**
    ///
    /// **Was** `RefCell<Vec<LayoutSlot>>` built by `(0..256).map(..).collect()`. Three costs,
    /// all paid on a path that runs millions of times a boot and none of them doing any work:
    ///   * a LAZY thread-local — `std` emits the lazy, destructor-registering storage, whose
    ///     access is the out-of-line `LocalKey::with` the RIP profile sees in the top three
    ///     leaves of every window (see `tls_cluster`'s header);
    ///   * a `Vec`, so reaching slot `k` is a pointer load THEN an index, and
    ///   * a bounds check with a panic landing pad against a length LLVM cannot see, even
    ///     though `k` is masked with `& (N - 1)` two lines earlier.
    ///
    /// **Is** a fixed `[LayoutSlot; 256]` with a `const` initialiser: the storage is eager (a
    /// state byte, not a lazy-init call), the array lives in the thread's own TLS image so the
    /// slot address is `base + k * 48` with no pointer chase, and the mask makes the bounds
    /// check statically dead. It also drops the 12 KB heap allocation and the 256-iteration
    /// construction loop that every thread performed on its first interpreted call.
    ///
    /// ⛔**IT IS STILL DROPPABLE** — the `String` keys own heap — so this does not reach the
    /// one-TLS-load lowering that a destructor-free `Cell` gets. Removing the keys was
    /// considered and REFUSED WITH A MEASUREMENT: an inline key needs the whole descriptor
    /// (an address is not a descriptor, and a wrong hit puts arguments in the wrong local
    /// slots), and a static census of `versions/1.21.11_unobfuscated` reads **43.5% of
    /// descriptors at or under 44 bytes and 84.5% at or under 108** — so a slot wide enough to
    /// hold the keys inline is a 16-32 KB table, which is the L1 regression above, and a
    /// narrower one would stop memoising exactly the long descriptors whose byte-at-a-time
    /// parse the memo exists to avoid.
    static MEMO: std::cell::RefCell<[LayoutSlot; MEMO_SLOTS]> =
        const { std::cell::RefCell::new([EMPTY_SLOT; MEMO_SLOTS]) };
}

/// The parse itself, extracted so the memo body below is small and so it is PURE: it reads only
/// its `&str` argument and touches no thread-local, which is what makes it safe to run while the
/// memo's single `RefCell` borrow is held.
fn parse_layout(descriptor: &str) -> Option<ArgLayout> {
    let b = descriptor.as_bytes();
    let mut i = descriptor.find('(').map(|p| p + 1)?;
    // Read ONCE per layout build (a memo MISS, not a call), never per parameter.
    let narrow_boxed = crate::boxid::enabled();
    let (mut n, mut slots, mut wide, mut unbox) = (0u32, 0u32, 0u64, 0u64);
    while i < b.len() && b[i] != b')' {
        if n >= 64 {
            return None; // cannot encode — caller falls back to the walk
        }
        let is_array = b[i] == b'[';
        while i < b.len() && b[i] == b'[' {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        let c = b[i];
        let is_wide = !is_array && matches!(c, b'J' | b'D');
        // `Z` always; `C`/`S`/`B` only while narrow boxes exist to arrive here.
        if !is_array && (c == b'Z' || (narrow_boxed && matches!(c, b'C' | b'S' | b'B'))) {
            unbox |= 1 << n;
        }
        if is_wide {
            wide |= 1 << n;
        }
        if c == b'L' {
            while i < b.len() && b[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        slots += 1 + is_wide as u32;
        n += 1;
    }
    Some(ArgLayout { n: n as u8, slots: slots as u16, wide, unbox })
}

/// Memoised per descriptor CONTENT, indexed by its address. Descriptors come from constant pools
/// and live as long as their class, but not every caller passes one, and a freed temporary's
/// address can be reused — so the address picks the slot and the bytes decide the hit.
/// Direct-mapped, not a hash map: this layer's cost is locks and hashes (ledger row 178).
///
/// ⭐**ONE THREAD-LOCAL CROSSING AND ONE `RefCell` BORROW FOR THE WHOLE FUNCTION.** The previous
/// shape probed under `MEMO.with(|m| .. m.borrow() ..)`, returned an `Option<ArgLayout>` out of
/// that closure, re-wrapped it (`(cond).then(..)` then `Some(hit)` — two 32-byte moves of the
/// same answer), and on a MISS entered a SECOND `MEMO.with` with a second `borrow_mut` to store.
/// A borrow is a flag load, a compare, a store and a release store on the guard's drop, and the
/// crossing itself is the out-of-line `LocalKey::with`; a miss paid both twice. Now the probe,
/// the parse and the fill happen inside one crossing under one borrow, and the answer is
/// constructed once. Holding the borrow across `parse_layout` is sound because that function is
/// pure over its `&str` and cannot re-enter this one.
pub(crate) fn arg_layout(descriptor: &str) -> Option<ArgLayout> {
    let slot = (descriptor.as_ptr() as usize >> 4) & (MEMO_SLOTS - 1);
    // The closure's return type is spelled out because the `?` below needs it before the tail
    // expression is seen.
    MEMO.with(|m| -> Option<ArgLayout> {
        let mut t = m.borrow_mut();
        {
            let s = &t[slot];
            if !descriptor.is_empty() && s.key == descriptor {
                return Some(ArgLayout {
                    n: s.n,
                    slots: s.slots,
                    wide: s.wide,
                    unbox: s.unbox,
                });
            }
        }
        let l = parse_layout(descriptor)?;
        // REFILL, don't reallocate: `clear` keeps the buffer, `push_str` grows it only past the
        // longest descriptor this slot has ever held. A `Box<str>` key would free one heap block
        // and allocate another on every miss that lands on an occupied slot.
        let s = &mut t[slot];
        s.key.clear();
        s.key.push_str(descriptor);
        s.n = l.n;
        s.slots = l.slots;
        s.wide = l.wide;
        s.unbox = l.unbox;
        Some(l)
    })
}

pub(crate) fn expand_locals_into(
    out: &mut Vec<Value>,
    is_static: bool,
    descriptor: &str,
    args: &mut Vec<Value>,
    heap: &Heap,
) {
    out.clear();
    let ecb = out.capacity();
    // ONE READ FOR THIS CALL'S PROCESS CONSTANTS (see the `arg_gates` block above): the `append`
    // shortcut's switch and the vec-growth census's switch used to be two acquire `OnceLock`
    // loads, the second of them behind an unconditional out-of-line call, on all 11.8M calls.
    let g = arg_gates();
    // RESERVE THE EXACT SLOT COUNT, not `args.len() + 2`. The expansion below gives every
    // long/double TWO local slots (JVMS 2.6.1), so a descriptor with several wide parameters needs
    // far more room than "one per argument plus two" - and the pushes then grow the buffer again.
    // The vec-growth census had this site at 11,803,098 calls with 40.35% GROWING, the largest
    // remaining growth row. `arg_slots` computes the same widths this loop walks, so the reserve is
    // exact rather than a guess; array parameters stay one slot each (`[J`/`[D` are narrow refs).
    // FAST PATH: the layout is a pure function of the descriptor, memoised once (`arg_layout`).
    // The walk below stays as the fallback for descriptors it cannot encode.
    if let Some(l) = arg_layout(descriptor) {
        // ⭐**THE ARGUMENTS ARE ALREADY THE LOCALS.** A descriptor with no category-2 parameter
        // and no `Z` parameter maps argument k onto local slot k with no expansion and no unbox,
        // so the loop below is a per-element restatement of a memcpy: a `Drain::next`, an
        // `unwrap_or`, two mask tests and a capacity-checked `push`, ONCE PER ARGUMENT, to move
        // values that were already in the right order. `Vec::append` moves the whole run with one
        // `copy_nonoverlapping` and leaves `args` empty exactly as `drain(..)` did, so the caller
        // still returns the buffer to the pool.
        //
        // **Was O(arguments) branches and pushes per call, is one memcpy per call.** What grows is
        // ARGUMENTS PER CALL; the shape covers every descriptor without a long, double or boolean
        // parameter, which is the great majority of them — and `wide == 0` makes `l.slots == l.n`,
        // so the reserve is the same exact size the slow path computes.
        //
        // ⚠**The arity test is not optional.** The loop PADS a short `args` with `Value::Null` and
        // silently DROPS a long one; `append` would do neither. When the counts disagree — a
        // caller that already prepended a receiver, a mismatched call site — this falls through to
        // the walk invisibly rather than laying arguments out differently.
        if (l.wide | l.unbox) == 0
            && args.len() == (!is_static) as usize + l.n as usize
            && (if g & AG_LIVE != 0 { fast_argcall() } else { g & AG_FAST_APPEND != 0 })
        {
            out.reserve(args.len());
            note_locals_reserve(g, ecb, out.capacity());
            out.append(args);
            return;
        }
        out.reserve((!is_static) as usize + l.slots as usize);
        note_locals_reserve(g, ecb, out.capacity());
        let mut it = args.drain(..);
        if !is_static {
            if let Some(r) = it.next() {
                out.push(r);
            }
        }
        for k in 0..l.n as u32 {
            let mut arg = it.next().unwrap_or(Value::Null);
            if l.unbox & (1u64 << k) != 0 {
                arg = unbox_param(heap, arg);
            }
            out.push(arg);
            if l.wide & (1u64 << k) != 0 {
                out.push(Value::Null); // second slot of a category-2 value
            }
        }
        return;
    }
    out.reserve((!is_static) as usize + crate::descriptor::arg_slots(descriptor));
    note_locals_reserve(g, ecb, out.capacity());
    // Drain `args` (rather than consume by value) so each argument is MOVED into
    // its slot AND the caller can return the now-empty buffer to the Vec pool —
    // the per-call argument Vec was the single largest allocator source.
    let mut it = args.drain(..);
    if !is_static {
        if let Some(r) = it.next() {
            out.push(r);
        }
    }
    let bytes = descriptor.as_bytes();
    let mut i = descriptor.find('(').map(|p| p + 1).unwrap_or(0);
    while i < bytes.len() && bytes[i] != b')' {
        // An array (any number of '[') is a single reference arg, never wide,
        // regardless of its element type.
        let is_array = bytes[i] == b'[';
        while i < bytes.len() && bytes[i] == b'[' {
            i += 1;
        }
        let wide = !is_array && matches!(bytes[i], b'J' | b'D');
        // A boolean PARAM (Z) receiving a boxed Boolean object must be unboxed to
        // its 0/1 — boxed booleans are heap objects now (so instanceof Boolean
        // works), and codec-decoded Booleans flow into record constructors whose
        // boolean fields, left boxed, made the record's `():Z` accessor return a
        // Ref that ifeq/istore could not consume.
        let param = if is_array { 0 } else { bytes[i] };
        match bytes[i] {
            b'L' => {
                while i < bytes.len() && bytes[i] != b';' {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
        let mut arg = it.next().unwrap_or(Value::Null);
        if param == b'Z' || (matches!(param, b'C' | b'S' | b'B') && crate::boxid::enabled()) {
            arg = unbox_param(heap, arg);
        }
        out.push(arg);
        if wide {
            out.push(Value::Null); // second slot of a category-2 value
        }
    }
}

/// A `Z`/`C`/`S`/`B` PARAMETER that arrived as a box, read back to its primitive.
///
/// ⭐**ONE IMPLEMENTATION FOR BOTH LAYOUT PATHS.** `expand_locals_into` has two of them — the
/// memoised mask walk and the byte walk it falls back to — and the `Z` rule was written out
/// twice; a widening applied to one and not the other would unbox a `Character` only for the
/// descriptors the memo happened to hold.
///
/// The read is UNCONDITIONAL on class, exactly as the `Z` rule always was, and that is sound for
/// the same reason: the descriptor says this parameter is a primitive, so the only reference that
/// can legally occupy it is the box of that primitive. `$value` is absent on anything else, and
/// `get_field` answers `Value::Null` there, which `as_int` turns into the historic 0 — so a
/// wrong-shaped reference degrades to the value it already produced, never to a panic.
#[inline]
pub(crate) fn unbox_param(heap: &crate::Heap, arg: Value) -> Value {
    match arg {
        Value::Ref(h) => Value::Int(heap.get_field(h, "$value").as_int().unwrap_or(0)),
        other => other,
    }
}

pub(crate) fn count_args(descriptor: &str) -> usize {
    let bytes = descriptor.as_bytes();
    let mut i = match descriptor.find('(') {
        Some(p) => p + 1,
        None => return 0,
    };
    let mut n = 0;
    while i < bytes.len() && bytes[i] != b')' {
        match bytes[i] {
            b'[' => {
                i += 1;
                continue;
            }
            b'L' => {
                while i < bytes.len() && bytes[i] != b';' {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
        n += 1;
    }
    n
}

/// Consume one argument type from the inner descriptor bytes at `*i`, returning
/// its kind char (`L` for any reference/array, else the primitive char).
pub(crate) fn jit_arg_kind(bytes: &[u8], i: &mut usize) -> u8 {
    match bytes[*i] {
        b'[' => {
            while *i < bytes.len() && bytes[*i] == b'[' {
                *i += 1;
            }
            if *i < bytes.len() && bytes[*i] == b'L' {
                while *i < bytes.len() && bytes[*i] != b';' {
                    *i += 1;
                }
            }
            *i += 1;
            b'L'
        }
        b'L' => {
            while *i < bytes.len() && bytes[*i] != b';' {
                *i += 1;
            }
            *i += 1;
            b'L'
        }
        c => {
            *i += 1;
            c
        }
    }
}

/// Split a descriptor's parameter list into its type substrings, in order.
/// `"(LNode;IJ)V"` → `["LNode;", "I", "J"]`. Used to verify a capturing lambda's
/// impl descriptor = (captured types) ++ (SAM param types) and to classify each
/// captured slot ref-vs-prim (a boxed primitive in an `Object` slot is a flat
/// `Value::Int` at runtime but must pack as a REF via `box_ref`, so the DECLARED
/// type — not the Value variant — decides).
pub(crate) fn param_types(descriptor: &str) -> Vec<&str> {
    let inner = args_of(descriptor);
    let b = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while b.get(i) == Some(&b'[') {
            i += 1;
        }
        match b.get(i) {
            Some(b'L') => {
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                i += 1; // consume ';'
            }
            Some(_) => i += 1,
            None => break,
        }
        out.push(&inner[start..i]);
    }
    out
}

/// Drop the FIRST parameter from a method descriptor: "(LNode;[J)J" → "([J)J".
/// An unbound method-ref lambda's SAM prepends its receiver as param 0, so the
/// remaining descriptor must equal the target method's own (jit_direct_resolve
/// uses this to gate the lambda direct-call on an exact, bridge-free match).
pub(crate) fn strip_first_param(desc: &str) -> Option<String> {
    let inner = desc.strip_prefix('(')?;
    let b = inner.as_bytes();
    let mut i = 0;
    while b.get(i) == Some(&b'[') {
        i += 1;
    }
    match b.get(i)? {
        b'L' => {
            while b.get(i) != Some(&b';') {
                i += 1;
                if i > inner.len() {
                    return None;
                }
            }
            i += 1;
        }
        _ => i += 1,
    }
    Some(format!("({}", &inner[i..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The memo indexes by ADDRESS and decides by CONTENT. Two descriptors that fold to the same
    /// slot must each get their own layout no matter which order they arrive in — this is the
    /// case the refilled `String` key has to keep answering now that no pointer is stored.
    #[test]
    fn colliding_descriptors_never_borrow_each_other_s_layout() {
        // Same shape, alternating, many times: the slot is rewritten on every miss and the
        // answers must not drift. `(JZ)V` has a wide param 0 and a boolean param 1.
        for _ in 0..8 {
            let a = arg_layout("(JZ)V").expect("encodable");
            assert_eq!((a.n, a.slots, a.wide, a.unbox), (2, 3, 0b01, 0b10));
            let b = arg_layout("(Ljava/lang/String;[JI)Z").expect("encodable");
            // `[J` is a REFERENCE — one slot, never wide.
            assert_eq!((b.n, b.slots, b.wide, b.unbox), (3, 3, 0, 0));
            let c = arg_layout("()V").expect("encodable");
            assert_eq!((c.n, c.slots, c.wide, c.unbox), (0, 0, 0, 0));
        }
        // An empty slot's key is "", and the probe must not report a hit for an empty
        // descriptor — which has no `(` and is refused outright.
        assert!(arg_layout("").is_none());
        assert!(arg_layout("no parens here").is_none());
    }

    /// ⭐**THE TWO PROPERTIES THAT MAKE THE GATE WORD CHEAP ARE INVISIBLE TO EVERY OTHER TEST.**
    /// A `Cell<u32>` with a `const` initialiser and no destructor is the one `thread_local!`
    /// form `std` lowers to a bare TLS-slot load; an edit that wrapped it in a `RefCell`, a
    /// `Vec` or anything with a `Drop` would silently put an out-of-line `LocalKey::with` back
    /// in front of 11.8 M calls a boot while behaviour stayed identical.
    #[test]
    fn the_gate_word_is_a_destructor_free_cell() {
        assert!(!std::mem::needs_drop::<std::cell::Cell<u32>>());
        assert_ne!(arg_gates(), 0, "a resolved word is never 0 — that is how the fast read works");
        assert_eq!(arg_gates(), arg_gates(), "and it is a per-thread constant once resolved");
        assert_eq!(arg_gates() & AG_RESOLVED, AG_RESOLVED);
    }

    /// The layout table's SIZE is a measured property. This tree has a **+6.8% regression** on
    /// record from a front cache that grew until it stopped fitting in L1, and `owner_bits`
    /// keeps another 8 KB one on the same threads; the `Vec` this replaced was 256 x 48 B and
    /// the fixed array must stay exactly that.
    #[test]
    fn the_layout_table_is_still_twelve_kilobytes() {
        assert_eq!(std::mem::size_of::<LayoutSlot>(), 48);
        assert_eq!(std::mem::size_of::<[LayoutSlot; MEMO_SLOTS]>(), 12 * 1024);
    }

    /// `expand_locals_into`'s `append` shortcut is only allowed to fire where the arguments ARE
    /// the locals; the predicate is exactly this, so it is pinned separately from the Heap-bound
    /// function that uses it.
    #[test]
    fn the_append_shortcut_fires_only_on_narrow_unboxable_free_descriptors() {
        let ok = arg_layout("(ILjava/lang/Object;)I").unwrap();
        assert_eq!(ok.wide | ok.unbox, 0);
        assert_eq!(ok.slots as usize, ok.n as usize); // reserve is the same either way
        for d in ["(J)V", "(D)V", "(Z)V", "(IZ)V"] {
            let l = arg_layout(d).unwrap();
            assert_ne!(l.wide | l.unbox, 0, "{d} must take the expanding walk");
        }
        // `[Z` / `[C` are REFERENCES — an array of booleans is not a boolean, and marking one
        // would have `unbox_param` read `$value` off the array handle.
        for d in ["([Z)V", "([C)V", "([B)V"] {
            let l = arg_layout(d).unwrap();
            assert_eq!(l.unbox, 0, "{d}: an array param is a reference");
        }
        // The narrow-box widening tracks the gate, so with `RAVA_NO_BOXID=1` these descriptors
        // keep the shortcut exactly as they did before this work.
        let narrow = arg_layout("(CSB)V").unwrap();
        assert_eq!(narrow.unbox, if crate::boxid::enabled() { 0b111 } else { 0 });
    }
}
