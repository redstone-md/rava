//! Runtime values on the operand stack and in local slots.
//!
//! Note: category-2 values (long/double) occupy a single slot in this early
//! model rather than the spec's two. That's correct for int/ref/string
//! programs (the current interpreter target) and is widened when the heap and
//! full type system land.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Reference-cell tag bits — the shared ABI between the JIT operand cells and the
/// heap's flat reference storage. A reference cell is: `0` = null, `h|REF_TAG` =
/// object handle, `h|ARRAY_TAG` = array handle, `idx|BOX_TAG` = a transient
/// jit_box_table entry (Str/boxed, per compiled call). Distinct high bits so a
/// cell is decoded purely by its tag.
pub(crate) const REF_TAG: i64 = 1 << 40;
pub(crate) const BOX_TAG: i64 = 1 << 41;
pub(crate) const ARRAY_TAG: i64 = 1 << 42;
/// PERSISTENT heap side-table index (`Heap::array_box`) for a reference-array
/// element that isn't a bare handle/null — Str/StdOut/StdErr/boxed primitive.
/// Distinct from the transient BOX_TAG so native aaload never feeds a persistent
/// index to the per-call jit_box_table.
pub(crate) const ABOX_TAG: i64 = 1 << 43;

/// ⛔**SUPERSEDED — THE SLOT IS 16 BYTES NOW (ledger rows 385-391).** What follows is the reasoning
/// that held while `Str` was a fat `Arc<str>`, kept because two of its three claims are still true
/// and the third is a worked example of an unmeasured premise surviving for a year.
///
/// **STILL TRUE:** 24 bytes is worse than 32 (indexing becomes `lea rax,[rax+rax*2]`, the move
/// splits into a 16- and an 8-byte store) — measured at 6/7 pairs, median −7.9%. And `align(64)`
/// is worse still: **+5.5%, 0/7 pairs, DISJOINT.** The rule behind both is that a slot wants to be
/// a POWER OF TWO, so the index is a shift and the move is aligned. **16 satisfies that too**, which
/// is why it was always the other candidate and never a compromise.
///
/// ⛔**WHAT WAS WRONG:** *"16 needs `Str` to stop being a fat pointer: ~500 construction sites, plus
/// a second pointer chase on every string read in a boot that is string-dense."* Both halves failed
/// when measured.
/// * **The site count**: a thin `RStr` newtype with `From`/`Deref` absorbs the `.into()` form nearly
///   every site already uses — the whole change was **FIVE compile errors** (row 390).
/// * **The density**: `RAVA_SLOT_CENSUS` on real Minecraft code — `ChunkFill`, **3,420,070,431**
///   operand pushes — reads **`Str` = 0.04%**, one push in 2,500, and **0.00%** on three compute
///   scenarios (row 388). The boot IS string-dense in its heap and its `toString` work; its
///   OPERAND TRAFFIC is not, and operand traffic is what the slot size costs.
///
/// ⇒ **Result**: bit-exact across 423 tests, five compiled gates and all 18 differential probes;
/// **peak RSS −2.10% with disjoint arms** (row 391); time 4/4 workloads favourable and never
/// disjoint on a box that drifted 38% within one arm.
///
/// ⇒ ⭐**The lesson this comment is now mainly for**: it rejected a lane on a number nobody had
/// counted, and the count took minutes. **A refusal needs a measurement as much as an admission
/// does.**

/// The interpreter's string slot: a THIN pointer, so `Value` can be 16 bytes instead of 32.
///
/// ⭐**WHY (ledger rows 385-388).** `Value` is `repr(align(32))` because its largest payload is
/// `Str(Arc<str>)` — a FAT pointer, 16 bytes — which pushes the payload to 24 and the slot to 32.
/// A 32-byte slot cannot live in a register and costs a wide move on every operand touch:
/// `examples/slotbench.rs` prices the same `match` at **5.02 ns/opcode on a 32-byte slot against
/// 3.89 on 16** (row 387), and the real interpreter runs at ~7.0 ns/opcode against `java -Xint`'s
/// ~1.0 (row 385).
///
/// ⛔**The reason this was refused before, and why it no longer holds.** `Value`'s own comment
/// rejected 16 bytes because a thin pointer adds an indirection "on every string read in a boot
/// that is string-dense". **That density was never measured.** `RAVA_SLOT_CENSUS` on real Minecraft
/// code (`ChunkFill`, **3,420,070,431** operand pushes) reads **`Str` = 0.04%** — one push in 2,500
/// — and **0.00%** on all three compute scenarios (row 388). The string-density claim is true of
/// the HEAP and of `toString` work, not of operand traffic, which is what the slot size costs.
///
/// The newtype exists so the ~242 construction sites keep compiling: `From<&str>`, `From<String>`
/// and `From<Arc<str>>` cover the `.into()` form nearly all of them already use, and `Deref<str>`
/// covers the readers.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RStr(std::sync::Arc<String>);

impl RStr {
    #[inline]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Same-allocation test, kept because the interner's fast path uses it to answer equality
    /// without comparing bytes (`opcodes::gates`).
    #[inline]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        std::sync::Arc::ptr_eq(&a.0, &b.0)
    }
}

impl std::ops::Deref for RStr {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for RStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl std::borrow::Borrow<str> for RStr {
    #[inline]
    fn borrow(&self) -> &str {
        self.0.as_str()
    }
}

impl From<&str> for RStr {
    #[inline]
    fn from(s: &str) -> Self {
        RStr(std::sync::Arc::new(s.to_string()))
    }
}

impl From<String> for RStr {
    #[inline]
    fn from(s: String) -> Self {
        RStr(std::sync::Arc::new(s))
    }
}

impl From<&String> for RStr {
    #[inline]
    fn from(s: &String) -> Self {
        RStr(std::sync::Arc::new(s.clone()))
    }
}

impl From<std::sync::Arc<str>> for RStr {
    #[inline]
    fn from(s: std::sync::Arc<str>) -> Self {
        RStr(std::sync::Arc::new(s.to_string()))
    }
}

impl From<std::borrow::Cow<'_, str>> for RStr {
    #[inline]
    fn from(s: std::borrow::Cow<'_, str>) -> Self {
        RStr(std::sync::Arc::new(s.into_owned()))
    }
}

impl std::fmt::Display for RStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.0.as_str(), f)
    }
}

impl std::fmt::Debug for RStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.0.as_str(), f)
    }
}

impl PartialEq<str> for RStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.0.as_str() == other
    }
}

impl PartialEq<&str> for RStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.0.as_str() == *other
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[repr(align(16))]
pub enum Value {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Str(RStr),
    /// Heap object handle (index into `Heap`).
    Ref(u32),
    /// Array handle (index into the heap's array table).
    Array(u32),
    /// DORMANT (Phase 1, rava-endgame): a direct pointer to an arena-allocated
    /// inline-cell object — the future replacement for `Ref(u32)`. Never
    /// constructed yet (the heap still hands out handles); present so the hybrid
    /// type exists and every exhaustive `match` is forced to acknowledge the
    /// raw-pointer future. Wired in Phase 2 (heap arena).
    Obj(crate::arena::ArenaPtr),
    Null,
    /// Sentinels for `System.out` / `System.err` until real PrintStreams exist.
    StdOut,
    StdErr,
}

/// ONE variant of eleven owns anything. The derived `Clone` is a match over all of them that
/// rustc kept out of line — `value::impl$2::clone` was 9.1% of a pure interpreter loop whose
/// values are all `Int`, because every `iload` calls it.
///
/// This is the same clone, written so the common case is a tag test and a whole-slot copy that
/// inlines into the caller. (It said "24-byte copy" while `Str` was a fat `Arc<str>`; the slot
/// has been 16 bytes since rows 385-391, and the pinning test below is what keeps it there.)
///
/// ⛔**AND THIS IS ALREADY THE CHEAP SHAPE — checked again, changed nothing.** The remaining
/// cost is the branch itself and `Value`'s drop glue, and both exist for one variant of eleven
/// that `RAVA_SLOT_CENSUS` prices at **0.04% of 3.42 G operand pushes**. Removing either means
/// `Str` stops owning a refcount, which is the 622-site / 99-file rewrite this tree has already
/// priced and closed. What is worth attacking is CALLERS that clone where they could move — and
/// those live on the operand-stack paths in `opcodes`/`frames`, not in this file.
impl Clone for Value {
    #[inline(always)]
    fn clone(&self) -> Value {
        match self {
            Value::Str(s) => Value::Str(s.clone()),
            // SAFETY: every other variant is plain data — i32/i64/f32/f64/u32/ArenaPtr/unit —
            // with no drop obligation, so duplicating the bits IS the clone. `Str` is excluded
            // above, and it is the only variant that owns a refcount.
            other => unsafe { std::ptr::read(other) },
        }
    }
}

impl Value {
    /// Is this value dispatchable as a functional-interface INSTANCE — i.e. the
    /// exact population the historic `matches!(v, Value::Ref(_))` callback gates
    /// meant to admit, plus the one representation they predate?
    ///
    /// Those gates were written when every object was a 0b01 handle. Under
    /// RAVA_ARENA_LAMBDA a CAPTURING closure is born as a raw 0b00 `Value::Obj`,
    /// so a `Ref`-only test reads it as "not a function" and SILENTLY DROPS the
    /// guest's work — the listener never fires, the `Files.find` predicate never
    /// filters, the comparator degrades to natural order. No error is raised;
    /// the world downstream is simply the wrong shape, which is precisely how a
    /// resource reload ends up handing a later stage an object of a class it did
    /// not expect. Representation is what such a gate must not be asking about.
    ///
    /// NOT `Ref | Obj`: `Value::Obj` is the ordinary arena object too, so the
    /// broad form would newly admit an arena-born ThreadGroup/Callable and change
    /// DEFAULT behaviour. The arena arm is narrowed to registered lambda classes,
    /// which makes the predicate byte-identical to the old one whenever the flag
    /// is off (the lambda class-id bitmap is empty ⇒ the arm is always false).
    #[inline]
    pub fn is_fn_value(&self) -> bool {
        match self {
            Value::Ref(_) => true,
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => crate::nextgen::is_arena_lambda(*p),
            _ => false,
        }
    }

    pub fn as_int(&self) -> Option<i32> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_long(&self) -> Option<i64> {
        match self {
            Value::Long(v) => Some(*v),
            Value::Int(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> f32 {
        match self {
            Value::Float(v) => *v,
            Value::Double(v) => *v as f32,
            Value::Long(v) => *v as f32,
            Value::Int(v) => *v as f32,
            _ => 0.0,
        }
    }

    pub fn as_f64(&self) -> f64 {
        match self {
            Value::Double(v) => *v,
            Value::Float(v) => *v as f64,
            Value::Long(v) => *v as f64,
            Value::Int(v) => *v as f64,
            _ => 0.0,
        }
    }

    pub fn as_i64(&self) -> i64 {
        self.as_long().unwrap_or(0)
    }

    pub fn as_i32(&self) -> i32 {
        match self {
            Value::Int(v) => *v,
            Value::Long(v) => *v as i32,
            Value::Float(v) => *v as i32,
            Value::Double(v) => *v as i32,
            _ => 0,
        }
    }

    /// Render as Java's `String.valueOf` would, for `println` output.
    pub fn display(&self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Long(v) => v.to_string(),
            Value::Float(v) => java_ftoa(*v),
            Value::Double(v) => java_dtoa(*v),
            Value::Str(s) => s.to_string(),
            Value::Ref(h) => format!("<object@{h}>"),
            Value::Array(h) => format!("<array@{h}>"),
            // An ArenaString renders as its content; other arena objects as a tag.
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
                String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)).into_owned()
            }
            Value::Obj(p) => format!("<arena-object@0x{:x}>", p.addr()),
            Value::Null => "null".to_string(),
            Value::StdOut => "<stdout>".to_string(),
            Value::StdErr => "<stderr>".to_string(),
        }
    }
}

/// Java `Double.toString`: shortest-roundtrip digits (Rust's formatter and the
/// JDK's Ryū agree), but Java's LAYOUT — plain decimal with at least one
/// fractional digit for 1e-3 <= |v| < 1e7 ("0.0", "450000.0"), computerized
/// scientific "d.dddE±x" (no '+') outside. Rust prints "0"/"450000"/"1e-7",
/// which diverges every string a guest builds from a double.
/// CENSUS (`RAVA_DTOA_CENSUS`, default OFF, inert): how many DISTINCT doubles does a boot render?
///
/// ⭐**Why this is the question.** Ledger rows 312-313: a boot renders **2,154,138 doubles for map
/// keys**, `java_dtoa` costs 1,766 ns of which **940 ns is two `format!` calls Rust charges 450 ns
/// each for**, and there is no fast dtoa in the tree to swap in. Both remaining designs are large —
/// a new dependency, or a native key encoding that must collide with every other list class.
///
/// But a double's rendering is a **pure function of its 64 bits**, so a memo on `to_bits()` needs
/// no soundness argument at all: it is the same function, answered from a table. That is only
/// worth building if the values REPEAT, and nothing in this tree has ever counted them.
thread_local! {
    /// Per-thread `java_dtoa` call count, advanced only while `RAVA_DTOA_CENSUS` is armed. Read it
    /// before and after a window to attribute the calls made inside it.
    static DTOA_N: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// This thread's `java_dtoa` call count (0 unless `RAVA_DTOA_CENSUS` is set).
#[cfg(feature = "next-gen-gc")]
pub fn dtoa_calls() -> u64 {
    DTOA_N.with(|c| c.get())
}

/// The census's own process constant. Reached from here ONLY under [`DG_LIVE`]; the default
/// path reads [`DG_CENSUS`] out of the word [`dtoa_gates`] loads once per render.
#[cfg(feature = "next-gen-gc")]
fn dtoa_census_armed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DTOA_CENSUS").is_some())
}

/// The gate AND the body, in the order they ran before this round — the kill switch's arm.
#[cfg(feature = "next-gen-gc")]
fn dtoa_census(v: f64) {
    if dtoa_census_armed() {
        dtoa_census_hit(v);
    }
}

/// The counting body, with no gate of its own. `#[cold] #[inline(never)]` because it is reached
/// on roughly zero of the ~2.15M renders a boot performs (ledger row 312) and its `HashSet` is a
/// droppable thread-local whose registration must not be laid out in `java_dtoa`'s prologue.
#[cfg(feature = "next-gen-gc")]
#[cold]
#[inline(never)]
fn dtoa_census_hit(v: f64) {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    thread_local! {
        static SEEN: std::cell::RefCell<std::collections::HashSet<u64>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    // ATTRIBUTION, not just a total. Row 317 left one contradiction standing: the memo should have
    // taken ~3.4 us out of the key-render arm if a render formats two doubles, and the arm moved
    // 4%. Either the ~300,000 calls come from somewhere else or the lists are not two elements
    // long. A per-thread call counter that a caller can read across a window decides which.
    DTOA_N.with(|c| c.set(c.get() + 1));
    let n = CALLS.fetch_add(1, Relaxed) + 1;
    let distinct = SEEN.with(|c| {
        let mut m = c.borrow_mut();
        m.insert(v.to_bits());
        m.len()
    });
    if n % 50_000 == 0 {
        eprintln!(
            "[DTOA] {n} renders, {distinct} distinct values on this thread ({:.1} renders per distinct)",
            n as f64 / distinct.max(1) as f64
        );
    }
}

/// MEMO on the bit pattern. **A double's decimal rendering is a pure function of its 64 bits**, so
/// this needs no soundness argument beyond that sentence: it is the same function, answered from a
/// table, and it cannot disagree with itself.
///
/// ⭐**Measured, in this order.** A boot renders **2,154,138 doubles for map keys** (row 312) and
/// `java_dtoa` costs **1,766 ns, of which 940 ns is two `format!` calls Rust charges ~450 ns each
/// for** (row 313) — so the two designs on the table were a new dependency or a native key
/// encoding that must collide with every other list class, both large. Then `RAVA_DTOA_CENSUS`
/// counted the population: **250,000 renders over TWENTY distinct values, 12,500 renders per
/// distinct.** A memo was never considered because nobody had counted; the count makes it the
/// cheapest sound option by an order of magnitude.
///
/// BOUNDED, like `stringify_memo` and the key-render cache: worldgen keys a handful of constants,
/// but nothing stops a workload from rendering millions of distinct doubles, and an unbounded
/// per-thread map is a slow leak rather than a cache. Clearing wholesale on overflow keeps the
/// common case a single lookup.
///
/// `RAVA_NO_DTOA_MEMO=1` restores the per-call render so this prices as a same-binary flag A/B.
#[cfg(feature = "next-gen-gc")]
const DTOA_MEMO_MAX: usize = 4096;

#[cfg(feature = "next-gen-gc")]
fn dtoa_memo_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_DTOA_MEMO").is_none())
}

// ── THE RENDER PATH'S PROCESS CONSTANTS, READ ONCE PER RENDER INSTEAD OF TWICE ─────────────
//
// ⭐**WHAT THE SHAPE WAS.** A boot renders **2,154,138 doubles** (ledger row 312), and the
// entry to every one of them was:
//
//   * `dtoa_census(v)` — an UNCONDITIONAL out-of-line call (the function is neither `#[inline]`
//     nor `#[cold]`, and it declares a droppable `HashSet` thread-local) whose first act is an
//     ACQUIRE `OnceLock` load saying "the census is off", followed by a return;
//   * `dtoa_memo_on()` — a SECOND acquire `OnceLock` load, immediately after.
//
// An acquire load is an optimisation barrier LLVM may not hoist and which forces the
// surrounding values to be materialised, so two of them plus a call stood in front of the
// memo lookup that is the whole point of the function.
//
// **Was: one out-of-line call and two acquire `OnceLock` loads per render. Is: one plain
// thread-local word load and two bit tests, with the census body `#[cold] #[inline(never)]`
// behind a bit that is clear in every non-diagnostic run.**
//
// Same shape as `opcodes::gates::op_gates` and `argcall::arg_gates`: a `Cell<u32>` with a
// `const` initialiser and no destructor is the one `thread_local!` form `std` lowers to a bare
// TLS-slot load with no lazy-init check and nothing to call. Neither gate here arms a thread or
// decides which path is REACHABLE — both are pure diagnostics/memo switches — so resolving them
// together at the entry moves no side effect.

/// Bit 0 — the word has been resolved on this thread (so a resolved word is never 0).
#[cfg(feature = "next-gen-gc")]
const DG_RESOLVED: u32 = 1 << 0;
/// Bit 1 — KILL SWITCH ENGAGED (`RAVA_NO_FAST_VALUE2=1` / `RAVA_NO_EMIT_MORE=1`): resolve
/// nothing here, and let each gate be read from its own `OnceLock` at its own site in its own
/// order, exactly as it was before this round.
#[cfg(feature = "next-gen-gc")]
const DG_LIVE: u32 = 1 << 1;
/// Bit 2 — [`dtoa_memo_on`]: the bit-pattern memo serves this process.
#[cfg(feature = "next-gen-gc")]
const DG_MEMO: u32 = 1 << 2;
/// Bit 3 — [`dtoa_census_armed`]: `RAVA_DTOA_CENSUS` is set, so every render is counted.
#[cfg(feature = "next-gen-gc")]
const DG_CENSUS: u32 = 1 << 3;

#[cfg(feature = "next-gen-gc")]
thread_local! {
    /// This thread's resolved `java_dtoa` gates. `Cell<u32>`, `const`-initialised and
    /// destructor-free — deliberately, that combination is what makes the read a plain load.
    static DTOA_GATES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The word. One plain per-thread load; never 0 on return.
#[cfg(feature = "next-gen-gc")]
#[inline(always)]
fn dtoa_gates() -> u32 {
    let g = DTOA_GATES.get();
    if g != 0 { g } else { dtoa_gates_cold() }
}

/// First touch on this thread: read the process constants through their `OnceLock`s — the only
/// place that still happens under the default — and cache the answers. Once per thread, so the
/// environment lookups behind them stay exactly as many as they were.
#[cfg(feature = "next-gen-gc")]
#[cold]
#[inline(never)]
fn dtoa_gates_cold() -> u32 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_VALUE2").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = DG_RESOLVED;
    if live {
        g |= DG_LIVE;
        DTOA_GATES.set(g);
        return g;
    }
    if dtoa_memo_on() {
        g |= DG_MEMO;
    }
    if dtoa_census_armed() {
        g |= DG_CENSUS;
    }
    DTOA_GATES.set(g);
    g
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    /// ⭐**`const`-INITIALISED.** It was `RefCell::new(FastMap::default())`, a LAZY initialiser,
    /// which makes `std` emit the lazy destructor-registering storage whose access is the
    /// out-of-line `LocalKey::with` the RIP profile finds in the top three leaves of every
    /// window (see `tls_cluster`'s header). `HashMap::with_hasher` and `BuildHasherDefault::new`
    /// are both `const fn`, so the same empty map can be built at compile time and the storage
    /// becomes the eager form — a state byte instead of a lazy-init call. It still owns heap
    /// (the `Box<str>` renders), so it does not reach the destructor-free lowering; nothing here
    /// can, because the rendered strings are variable-length and must be owned.
    static DTOA_MEMO: std::cell::RefCell<crate::FastMap<u64, Box<str>>> = const {
        std::cell::RefCell::new(std::collections::HashMap::with_hasher(
            std::hash::BuildHasherDefault::new(),
        ))
    };
}

/// The memo itself — probe, render, freeze. Split out of [`java_dtoa`] so the gate word decides
/// between three whole arms instead of guarding statements inside one body.
#[cfg(feature = "next-gen-gc")]
fn java_dtoa_memoised(v: f64) -> String {
    let bits = v.to_bits();
    if let Some(hit) = DTOA_MEMO.with(|c| c.borrow().get(&bits).map(|s| s.to_string())) {
        return hit;
    }
    let rendered = java_dtoa_uncached(v);
    // FULL MEANS STOP, NOT CLEAR. Clearing wholesale is what the sibling caches do, and it
    // is wrong here: a workload with millions of DISTINCT doubles then pays an insert and a
    // periodic clear on every render. Measured on exactly that corpus (2 M distinct values,
    // `--example dtoabench`), the clearing form was **+6% against no memo at all** — the
    // "a probe that misses is not free" shape, ledger row 240. Freezing the table instead
    // leaves a miss costing one failed lookup, and the boot's population is TWENTY values,
    // so it is never reached in the case this exists for.
    DTOA_MEMO.with(|c| {
        let mut m = c.borrow_mut();
        if m.len() < DTOA_MEMO_MAX {
            m.insert(bits, rendered.as_str().into());
        }
    });
    rendered
}

pub fn java_dtoa(v: f64) -> String {
    #[cfg(feature = "next-gen-gc")]
    {
        let g = dtoa_gates();
        if g & DG_LIVE != 0 {
            // KILL SWITCH: byte-for-byte the order this round replaced — the census's own gate
            // read at its own site, then the memo's.
            dtoa_census(v);
            if dtoa_memo_on() {
                return java_dtoa_memoised(v);
            }
            return java_dtoa_uncached(v);
        }
        if g & DG_CENSUS != 0 {
            dtoa_census_hit(v);
        }
        if g & DG_MEMO != 0 {
            return java_dtoa_memoised(v);
        }
    }
    java_dtoa_uncached(v)
}

/// The rendering itself — the JDK's rule, unchanged. `java_dtoa` is now its cache.
pub fn java_dtoa_uncached(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    java_fp_layout(&java_significand(&format!("{v:e}"), |p| format!("{v:.p$e}")))
}

/// Java `Float.toString` — same layout rules over the f32 shortest digits.
pub fn java_ftoa(v: f32) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    java_fp_layout(&java_significand(&format!("{v:e}"), |p| format!("{v:.p$e}")))
}

/// "Shortest that round-trips" is only the FIRST half of Java's rule, and Rust's
/// shortest form differs from the JDK on both counts:
///
///   * The javadoc takes p = the minimal length over the decimals that round to v,
///     "but if p < 2, let T be the set of decimals of length 1 or 2". So a value
///     whose shortest form is a single digit is re-rendered with two when that
///     lands nearer v. `Double.MIN_VALUE` is 4.9406…E-324: Rust prints "5e-324"
///     (it round-trips), the JDK prints "4.9E-324". Same for "1.4E-45".
///   * Among the decimals of that length, the JDK takes the one CLOSEST to v, and
///     on an exact tie the one with the EVEN last digit. Ties are not exotic — any
///     double whose ulp is a power of ten's worth of trailing 25/75 hits one:
///     1658206780088562.25 sits exactly between …562.2 and …562.3, where the JDK
///     answers …562.2 and Rust's shortest answers …562.3. That was ~0.05% of a
///     150k-value sweep.
///
/// Both fall out of asking Rust for the correctly-rounded rendering at a FIXED
/// length instead — `format_exact` rounds half-to-even, which is exactly the
/// tie-break the javadoc specifies. Only the length comes from the shortest form.
/// A trailing zero survives when the nearest decimal of that length is really a
/// shorter one (1.0e2 IS the decimal 1x10^2); it is dropped so the layout rules
/// below see the true significand, or 0.001 would come out "0.0010".
fn java_significand(shortest: &str, exact: impl Fn(usize) -> String) -> String {
    let n = shortest.split(['e', 'E']).next().unwrap_or("")
        .bytes().filter(u8::is_ascii_digit).count();
    let rendered = exact(n.max(2) - 1);
    match rendered.split_once(['e', 'E']) {
        Some((m, e)) if m.ends_with('0') => {
            format!("{}e{e}", m.trim_end_matches('0').trim_end_matches('.'))
        }
        _ => rendered,
    }
}

/// The same digit selection, laid out PLAIN — never in exponent form, and with no
/// forced ".0". `%f`-style formatting needs the digit STRING in positional order,
/// and reading it off Rust's `Display` (which also never uses an exponent) picks
/// up Rust's tie-break: `%.1f` of 1623652094776343.25 came out ".3" where the JDK
/// says ".2".
pub fn java_plain_decimal(v: f64) -> String {
    if !v.is_finite() || v == 0.0 {
        return format!("{v}");
    }
    let sci = java_significand(&format!("{v:e}"), |p| format!("{v:.p$e}"));
    let (mant, exp) = sci.split_once(['e', 'E']).unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let sign = if mant.starts_with('-') { "-" } else { "" };
    let digits: String = mant.bytes().filter(u8::is_ascii_digit).map(char::from).collect();
    if exp < 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
    } else {
        let e = exp as usize;
        if digits.len() > e + 1 {
            format!("{sign}{}.{}", &digits[..=e], &digits[e + 1..])
        } else {
            format!("{sign}{digits}{}", "0".repeat(e + 1 - digits.len()))
        }
    }
}

/// Java `Double.toHexString`: the IEEE-754 fields spelled out directly —
/// "0x1.<13 hex fraction digits>p<unbiased exponent>" for a normal, "0x0.…p-1022"
/// for a subnormal, with trailing fraction zeros dropped but never the last digit.
/// Rust's `{:x}` on a float does not exist and `{:a}`-style formatting is not in
/// std, so this is written out rather than delegated.
pub fn java_hex_double(v: f64) -> String {
    if v.is_nan() { return "NaN".to_string(); }
    if v.is_infinite() { return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() }; }
    let bits = v.to_bits();
    let sign = if bits >> 63 == 1 { "-" } else { "" };
    let exp = ((bits >> 52) & 0x7FF) as i32;
    let frac = bits & 0x000F_FFFF_FFFF_FFFF;
    let mut digits = format!("{frac:013x}");
    while digits.len() > 1 && digits.ends_with('0') { digits.pop(); }
    match (exp, frac) {
        (0, 0) => format!("{sign}0x0.0p0"),
        (0, _) => format!("{sign}0x0.{digits}p-1022"),
        _ => format!("{sign}0x1.{digits}p{}", exp - 1023),
    }
}

/// Java `Float.toHexString`. The 23 fraction bits pad LEFT-ALIGNED to 24 (six hex
/// digits), which is why the field is shifted up by one before it is printed —
/// `Float.toHexString(0.1f)` is "0x1.99999ap-4", not "0x1.4ccccdp-4".
pub fn java_hex_float(v: f32) -> String {
    if v.is_nan() { return "NaN".to_string(); }
    if v.is_infinite() { return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() }; }
    let bits = v.to_bits();
    let sign = if bits >> 31 == 1 { "-" } else { "" };
    let exp = ((bits >> 23) & 0xFF) as i32;
    let frac = bits & 0x007F_FFFF;
    let mut digits = format!("{:06x}", frac << 1);
    while digits.len() > 1 && digits.ends_with('0') { digits.pop(); }
    match (exp, frac) {
        (0, 0) => format!("{sign}0x0.0p0"),
        (0, _) => format!("{sign}0x0.{digits}p-126"),
        _ => format!("{sign}0x1.{digits}p{}", exp - 127),
    }
}

/// Re-layout Rust's `{:e}` form ("d.ddde±x") per Java's toString rules. The
/// digits are already the shortest roundtrip set; only placement changes.
fn java_fp_layout(sci: &str) -> String {
    let (mant, exp) = sci.split_once(['e', 'E']).unwrap_or((sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    // Java: plain decimal iff 10^-3 <= |v| < 10^7, i.e. first-digit exponent in [-3, 6].
    if (-3..7).contains(&exp) {
        if exp >= 0 {
            let e = exp as usize;
            let int_part: String = if digits.len() > e {
                digits[..=e].to_string()
            } else {
                format!("{digits}{}", "0".repeat(e + 1 - digits.len()))
            };
            let frac = if digits.len() > e + 1 { &digits[e + 1..] } else { "0" };
            format!("{sign}{int_part}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let head = &digits[..1];
        let tail = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{sign}{head}.{tail}E{exp}")
    }
}

#[cfg(test)]
mod fp_tests {
    use super::{java_dtoa, java_ftoa};

    #[test]
    fn dtoa_matches_java_layout() {
        assert_eq!(java_dtoa(0.0), "0.0");
        assert_eq!(java_dtoa(-0.0), "-0.0");
        assert_eq!(java_dtoa(1.0), "1.0");
        assert_eq!(java_dtoa(450000.0), "450000.0");
        assert_eq!(java_dtoa(1.5), "1.5");
        assert_eq!(java_dtoa(0.001), "0.001");
        assert_eq!(java_dtoa(1e-4), "1.0E-4");
        assert_eq!(java_dtoa(1e7), "1.0E7");
        assert_eq!(java_dtoa(9999999.5), "9999999.5");
        assert_eq!(java_dtoa(123456789.0), "1.23456789E8");
        assert_eq!(java_dtoa(-2.5e-9), "-2.5E-9");
        assert_eq!(java_dtoa(f64::NAN), "NaN");
        assert_eq!(java_dtoa(f64::INFINITY), "Infinity");
        assert_eq!(java_dtoa(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(java_dtoa(0.1), "0.1");
        assert_eq!(java_ftoa(0.0f32), "0.0");
        assert_eq!(java_ftoa(3.14f32), "3.14");
        assert_eq!(java_ftoa(1e7f32), "1.0E7");
        // p < 2: the JDK re-renders with two digits when that lands nearer v.
        assert_eq!(java_dtoa(f64::MIN_POSITIVE * f64::EPSILON), "4.9E-324");
        assert_eq!(java_ftoa(f32::from_bits(1)), "1.4E-45");
        assert_eq!(java_dtoa(1e23), "1.0E23");
        assert_eq!(java_dtoa(100.0), "100.0");
    }
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod dtoa_gate_tests {
    use super::*;

    /// ⭐**THE PROPERTY THAT MAKES THE GATE WORD CHEAP IS INVISIBLE TO EVERY OTHER TEST.** A
    /// `Cell<u32>` with a `const` initialiser and no destructor is the one `thread_local!` form
    /// `std` lowers to a bare TLS-slot load; an edit that made it droppable would silently put
    /// an out-of-line `LocalKey::with` back in front of ~2.15 M renders a boot (ledger row 312)
    /// while every rendering test still passed.
    #[test]
    fn the_gate_word_is_a_destructor_free_cell() {
        assert!(!std::mem::needs_drop::<std::cell::Cell<u32>>());
        assert_ne!(dtoa_gates(), 0, "a resolved word is never 0 — that is how the fast read works");
        assert_eq!(dtoa_gates(), dtoa_gates(), "and it is a per-thread constant once resolved");
        assert_eq!(dtoa_gates() & DG_RESOLVED, DG_RESOLVED);
    }

    /// The memo may only ever answer what the renderer would have. Folding the two gates into
    /// one word changed WHERE they are read, never what they say, so the memoised and the
    /// uncached renders must agree on every value — including the ones the layout rules split.
    #[test]
    fn the_memo_never_disagrees_with_the_renderer() {
        for v in [
            0.0, -0.0, 1.0, 1.5, 0.001, 1e-4, 1e7, 9999999.5, 123456789.0, -2.5e-9, 450000.0,
            1e23, 100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY,
        ] {
            let a = java_dtoa(v);
            let b = java_dtoa_uncached(v);
            assert_eq!(a, b, "memo disagreed on {v:?}");
            // ...and again, now that the value is certainly in the table.
            assert_eq!(java_dtoa(v), b, "second render of {v:?} drifted");
        }
    }
}

#[cfg(test)]
mod slot_size_probe {
    /// The interpreter's operand stack and locals are `Vec<Value>`, so `Value`'s SIZE and its
    /// `Drop` are the per-slot cost of every push, pop, frame build and frame teardown. HotSpot's
    /// interpreter slot is 8 untyped bytes with no destructor. This prints ours so the comparison
    /// is a number rather than an assumption.
    #[test]
    fn print_value_slot_cost() {
        eprintln!("size_of::<Value>()   = {}", std::mem::size_of::<crate::Value>());
        eprintln!("align_of::<Value>()  = {}", std::mem::align_of::<crate::Value>());
        eprintln!("needs_drop::<Value>()= {}", std::mem::needs_drop::<crate::Value>());
    }
}

#[cfg(test)]
mod slot_size_tests {
    /// ⭐**THE SLOT SIZE IS A MEASURED PROPERTY, SO IT GETS A TEST.** Rows 385-388: the interpreter
    /// runs at ~7 ns/opcode against `java -Xint`'s ~1, the operand slot is the largest identified
    /// cause, and `slotbench` prices 32 → 16 bytes at **1.29×**. That win is one fat field away from
    /// being lost again — anything with a 16-byte payload (`Arc<str>`, `&[T]`, `(u64, u64)`) silently
    /// pushes `Value` back to 32 and nothing else would notice.
    #[test]
    fn value_is_a_sixteen_byte_slot() {
        assert_eq!(
            std::mem::size_of::<crate::value::Value>(),
            16,
            "Value grew past 16 bytes — some variant gained a payload larger than 8.              The operand slot size is worth 1.29x on pure dispatch (ledger row 387);              box the new payload or make it thin, do not widen the slot."
        );
        // The string slot is the one that used to be fat.
        assert_eq!(std::mem::size_of::<crate::value::RStr>(), 8);
    }
}
