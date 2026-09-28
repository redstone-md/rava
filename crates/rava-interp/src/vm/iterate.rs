//! Iteration and comparison over guest values.

use crate::*;

/// Kill switch for this round's iteration work: `RAVA_NO_FAST_ITER=1` (or the blanket
/// `RAVA_NO_EMIT_MORE`) puts every drain back on the by-name callee lookup it used before.
/// Read once per process.
#[inline]
pub(crate) fn fast_iter() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_ITER").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The index of `name+descriptor` inside its DECLARING class's `methods` table, so a drain can
/// hand it to `invoke_idx` once instead of re-deriving it once per element.
///
/// ⭐**THIS IS THE PER-ELEMENT CONSTANT OF AN ITERATOR.** `hasNext`/`next`/`get` resolve to the
/// same `MemberInfo` for every step of a given drain, yet `Vm::invoke` — which is literally
/// `invoke_idx(&class, None, …)` behind an OWNED `Arc` parameter — charged each step for both
/// halves of that: an `Arc<LoadedClass>` refcount increment and decrement (a pair this tree
/// prices at 12 ns against a 105 ns call), and `ClassFile::method`, which hashes the name plus
/// the descriptor and then memcmps both against the overload set to confirm the slot.
/// **Was O(1) hash + 2 string compares + 2 atomic RMW per ELEMENT; is a `Vec` index and no
/// refcount traffic at all.** What grows: elements drained per collection.
///
/// **TABLE SIZE ADDED: zero.** This is address arithmetic over a `Vec` the class already owns —
/// `ClassFile::method` returns `&self.methods[i]`, so the byte offset from the slice base IS
/// `i * size_of::<MemberInfo>()`. No deref of the raw pointer, so no `unsafe`; O(1), where a
/// `position` scan would be O(methods-per-class) and would LOSE on a two-element drain.
fn iter_midx(
    class: &Arc<crate::class_loader::LoadedClass>,
    name: &str,
    descriptor: &str,
) -> Option<u32> {
    if !fast_iter() {
        return None;
    }
    let ms = &class.file.methods;
    let m = class.file.method(name, descriptor)?;
    let off = (m as *const rava_classfile::MemberInfo as usize).checked_sub(ms.as_ptr() as usize)?;
    let sz = std::mem::size_of::<rava_classfile::MemberInfo>();
    if sz == 0 || off % sz != 0 {
        return None;
    }
    // ⛔**VERIFY, NEVER TRUST.** `invoke_idx` does NOT re-check the name behind a `Some(i)` — it
    // indexes and runs — so a wrong index would silently call a DIFFERENT method, which is the
    // dangerous shape of bug rather than a crash. A failed check answers `None` and the callee is
    // looked up by name exactly as it always was.
    let mi = ms.get(off / sz)?;
    (mi.name == name && mi.descriptor == descriptor).then_some((off / sz) as u32)
}

// ═══════════════ HR-COMPARE: derive the comparator once, order strings without allocating ═══════
//
// `Vm::compare` is 1.82% of the samples taken under compiled bodies on the bootstrap bench, and
// nothing it derives per call is a property of the two ELEMENTS:
//
//   * the comparator's kind — `is_lambda`, then a `$cmp` field read (a name fold, an L1 probe,
//     a slot scan and an `Arc` clone+drop of the tag string) and a ten-arm string match — is a
//     constant of the COMPARATOR OBJECT, and a sort of N elements asks it N log N times;
//   * the sub-comparators of a `then`/`reversed`/`nullsFirst` tree and the `$keyfn` of a
//     `comparing` are fields put ONCE at construction (every producer — `reflect::concurrent`,
//     `opcodes::gates`, `map::dispatch`, `natives::misc` — writes them before the handle is
//     ever returned) and read again at every level of every comparison;
//   * `natural_cmp` orders two strings by building TWO owned `String`s (`display()`) and
//     dropping them — two mallocs per comparison for the population the registry sorts are
//     made of.
//
// DERIVE ONCE: a per-thread 8-slot memo keyed by the comparator HANDLE holds the derived kind
// and up to two sub-handles. A slot is believed only while (a) `heap::handle_epoch()` is the
// value it was stamped with — that counter is bumped on every retired object slot, which is
// the only way a live handle can come to name a different object inside one heap — and (b)
// the object's stable `Box` address is the one seen at derivation, which additionally catches
// a heap torn down and rebuilt on the same thread (tests) unless the allocator hands the new
// object the same address at the same index with no retirement in between. The `then` arm
// re-checks the epoch after its first clause ran guest code and re-reads `$second` by name if
// it moved, so a hit is never trusted across a collection.
//
// GUARD — what can only REJECT: a comparator that is an arena object (`Value::Obj`, which a
// collection moves), a `ciorder`, a sub-comparator or key function that is not a heap handle
// or null, or a slot with no object behind it, is not memoised; the historic path below runs
// unchanged. An element pair that is not (int|long)², (double)², (float)², or (string)² is
// ordered by the historic `natural_cmp` — same function, same answer.
//
// JAVA ORDER, where the fast arm serves: strings by UTF-16 code unit (the historic byte order
// is code-point order, which differs only for a supplementary character against U+E000..FFFF);
// doubles/floats by `Double.compare`/`Float.compare` (-0.0 < 0.0, NaN greatest and equal to
// itself); ints and longs numerically, exactly as before.
//
// KILL SWITCH: `RAVA_NO_HR_COMPARE=1` (or the blanket `RAVA_NO_EMIT_MORE`) makes `compare`
// the historic function, byte for byte. Resolved once.

const HR_ON: u8 = 1;
const HR_PROBE: u8 = 2;

/// Bit 0: the block is on. Bit 1: `RAVA_JIT_PIC_PROBE` is armed, so the census counters run.
/// One `OnceLock` read per compare — the switch read this function has to make anyway.
#[inline]
fn hr_gate() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let on = std::env::var_os("RAVA_NO_HR_COMPARE").is_none()
            && !crate::flags::emit_more_disabled();
        (on as u8) | ((crate::picprobe::enabled() as u8) << 1)
    })
}

// Comparator kinds a memo slot can carry. `K_NONE` is an empty slot; `K_DECLINE` remembers
// that this handle's shape was refused, so the refusal costs one probe rather than a
// re-derivation per comparison.
const K_NONE: u8 = 0;
const K_DECLINE: u8 = 1;
const K_LAMBDA: u8 = 2;
const K_NATURAL: u8 = 3;
const K_REVERSE: u8 = 4;
const K_KEY: u8 = 5;
const K_KEYREV: u8 = 6;
const K_ENTRYKEY: u8 = 7;
const K_ENTRYVAL: u8 = 8;
const K_NULLSFIRST: u8 = 9;
const K_NULLSLAST: u8 = 10;
const K_REVERSED: u8 = 11;
const K_THEN: u8 = 12;
const K_COUNT: usize = 13;

/// `nullsFirst(null)` / `nullsLast(null)`: all non-nulls compare equal, no inner comparator.
const F_NOCMP: u8 = 1;
/// The comparator handle carries no `$cmp` tag (census only; the arm is the historic natural one).
const F_UNTAGGED: u8 = 2;

/// A sub-comparator or key function stored as a handle; `SUB_NULL` stands for `Value::Null`
/// (a `then` built without a second clause, a `comparing` without a key function), which the
/// historic path hands to the recursive `compare`/`apply` as a null and which this path hands
/// over identically.
const SUB_NULL: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct CmpMemo {
    handle: u32,
    kind: u8,
    flags: u8,
    x: u32,
    y: u32,
    /// `heap::handle_epoch()` at derivation.
    epoch: u64,
    /// The comparator object's stable `Box<Object>` address at derivation.
    addr: usize,
}

impl CmpMemo {
    const EMPTY: CmpMemo =
        CmpMemo { handle: 0, kind: K_NONE, flags: 0, x: 0, y: 0, epoch: 0, addr: 0 };
}

const MEMO_SLOTS: usize = 8;

std::thread_local! {
    // Outer `const { … }` so the macro takes its const arm: `CmpMemo` is `Copy`, the cells
    // need no destructor, and the table lowers to a bare `#[thread_local]` static — one
    // TLS-base read and a displacement per probe (see `syn_field_id` for the same shape).
    static CMP_MEMO: [std::cell::Cell<CmpMemo>; MEMO_SLOTS] =
        const { [const { std::cell::Cell::new(CmpMemo::EMPTY) }; MEMO_SLOTS] };
}

#[inline]
fn memo_slot(h: u32) -> usize {
    (h as usize) & (MEMO_SLOTS - 1)
}

/// The stable address of the object behind `h`, or `None` for an empty/dead slot.
#[inline]
fn object_addr(heap: &crate::heap::Heap, h: u32) -> Option<usize> {
    // Phase 5: `Heap::get` is the same two probes (`objects.get` + `ObjectSlot::get`),
    // reached through the `&self` facade so the handle table stays private to `heap/`.
    heap.get(h).map(|o| o as *const _ as usize)
}

#[inline]
fn sub_of(v: &Value) -> Option<u32> {
    match v {
        Value::Ref(h) => Some(*h),
        Value::Null => Some(SUB_NULL),
        _ => None,
    }
}

#[inline]
fn sub_value(x: u32) -> Value {
    if x == SUB_NULL { Value::Null } else { Value::Ref(x) }
}

// ── census (RAVA_JIT_PIC_PROBE; relaxed adds, only behind the probe bit) ──
const C_HIT: usize = 0; // memo hit
const C_MISS: usize = 1; // derivation (memo miss)
const C_DECLINED: usize = 2; // comparator shape refused -> historic path
const C_NAT_FAST: usize = 3; // element pair ordered without allocating
const C_NAT_SLOW: usize = 4; // element pair sent to the historic `natural_cmp`
const C_ARENA_CMP: usize = 5; // comparator was an arena object -> historic path
const C_BASE: usize = 6; // + kind
static HR_N: [std::sync::atomic::AtomicU64; C_BASE + K_COUNT] =
    [const { std::sync::atomic::AtomicU64::new(0) }; C_BASE + K_COUNT];

#[inline]
fn hr_count(gate: u8, i: usize) {
    if gate & HR_PROBE != 0 {
        HR_N[i].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// `[HR-CMP]` — the population this block serves, for the periodic profiler dump. Empty when
/// nothing was counted (probe off, or no comparison ran).
pub fn hr_compare_report() -> String {
    // Two censuses share the one dump hook: the HR memo's (probe-gated) and the caller census
    // (always on). Either may be empty; a newline joins them when both have rows.
    let hr = hr_memo_report();
    let cc = cmp_caller_report();
    match (hr.is_empty(), cc.is_empty()) {
        (true, true) => String::new(),
        (false, true) => hr,
        (true, false) => cc,
        (false, false) => format!("{hr}\n{cc}"),
    }
}

/// `[HR-CMP]` proper — see `hr_compare_report`.
fn hr_memo_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let n = |i: usize| HR_N[i].load(Relaxed);
    let served: u64 = (K_LAMBDA as usize..K_COUNT).map(|k| n(C_BASE + k)).sum();
    if served + n(C_DECLINED) + n(C_ARENA_CMP) == 0 {
        return String::new();
    }
    const NAMES: [&str; K_COUNT] = [
        "-", "decline", "lambda", "natural", "reverse", "key", "keyrev", "entrykey", "entryval",
        "nullsfirst", "nullslast", "reversed", "then",
    ];
    let mut out = format!(
        "[HR-CMP] compares served={served} (memo hit={} derive={}) | to historic: shape={} arena-cmp={} | natural pairs: no-alloc={} historic={}",
        n(C_HIT), n(C_MISS), n(C_DECLINED), n(C_ARENA_CMP), n(C_NAT_FAST), n(C_NAT_SLOW)
    );
    let mut rows: Vec<(u64, &str)> = (K_LAMBDA as usize..K_COUNT)
        .map(|k| (n(C_BASE + k), NAMES[k]))
        .filter(|r| r.0 > 0)
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    for (c, name) in rows {
        out.push_str(&format!("\n[HR-CMP]   {c:>10}  {name}"));
    }
    out
}

#[inline]
fn sign(o: std::cmp::Ordering) -> i32 {
    match o {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// `Double.compare`: `<`/`>` first, then `doubleToLongBits` as a signed long — which puts
/// -0.0 below 0.0 and every NaN (canonicalised to 0x7ff8...) above +Infinity and equal to itself.
#[inline]
fn java_double_compare(x: f64, y: f64) -> i32 {
    if x < y {
        return -1;
    }
    if x > y {
        return 1;
    }
    let bits = |v: f64| -> i64 { if v.is_nan() { 0x7ff8_0000_0000_0000 } else { v.to_bits() as i64 } };
    sign(bits(x).cmp(&bits(y)))
}

/// `Float.compare`, same construction (canonical NaN 0x7fc00000).
#[inline]
fn java_float_compare(x: f32, y: f32) -> i32 {
    if x < y {
        return -1;
    }
    if x > y {
        return 1;
    }
    let bits = |v: f32| -> i32 { if v.is_nan() { 0x7fc0_0000 } else { v.to_bits() as i32 } };
    sign(bits(x).cmp(&bits(y)))
}

/// The code point led by `b[0]`, from its own bytes only. `None` for a malformed lead or a
/// truncated sequence — callers hand this VALID UTF-8, so `None` is a refusal, not an answer.
#[inline]
fn lead_cp(b: &[u8]) -> Option<u32> {
    let l = *b.first()? as u32;
    let c = |i: usize| -> Option<u32> { b.get(i).map(|v| *v as u32 & 0x3F) };
    Some(match l {
        0x00..=0x7F => l,
        0xC0..=0xDF => (l & 0x1F) << 6 | c(1)?,
        0xE0..=0xEF => (l & 0x0F) << 12 | c(1)? << 6 | c(2)?,
        0xF0..=0xF7 => (l & 0x07) << 18 | c(1)? << 12 | c(2)? << 6 | c(3)?,
        _ => return None,
    })
}

/// Java `String.compareTo` order — by UTF-16 code unit — over two VALID UTF-8 byte strings,
/// with no allocation, as -1/0/1.
///
/// Byte order on UTF-8 is code-point order, and code-point order agrees with UTF-16 code-unit
/// order except when a supplementary character (a surrogate pair, D800..DFFF) meets a BMP
/// character in E000..FFFF. So: find the first differing byte; if both are ASCII that byte
/// decides; otherwise back up to the start of the character containing it — the same offset
/// in both strings, since the prefixes are identical and a lead byte fixes its own length —
/// decode both, and compare their FIRST UTF-16 units, then the code points.
fn utf16_order(a: &[u8], b: &[u8]) -> Option<i32> {
    let n = a.len().min(b.len());
    let i = match a[..n].iter().zip(&b[..n]).position(|(x, y)| x != y) {
        None => return Some(sign(a.len().cmp(&b.len()))),
        Some(i) => i,
    };
    let (x, y) = (a[i], b[i]);
    if x < 0x80 && y < 0x80 {
        return Some(if x < y { -1 } else { 1 });
    }
    let mut s = i;
    while s > 0 && (a[s] & 0xC0) == 0x80 {
        s -= 1;
    }
    let (ca, cb) = (lead_cp(&a[s..])?, lead_cp(&b[s..])?);
    let unit = |cp: u32| -> u32 { if cp >= 0x1_0000 { 0xD800 + ((cp - 0x1_0000) >> 10) } else { cp } };
    let (ua, ub) = (unit(ca), unit(cb));
    if ua != ub {
        return Some(if ua < ub { -1 } else { 1 });
    }
    // Same high surrogate: the low surrogates order as the code points do.
    Some(sign(ca.cmp(&cb)))
}

/// The UTF-8 bytes of a string value — a handle string, or an arena string whose bytes are
/// valid UTF-8 (the historic `display()` runs `from_utf8_lossy` over them, which is the
/// identity exactly then; anything else is refused to it).
#[inline]
fn str_bytes(v: &Value) -> Option<&[u8]> {
    match v {
        Value::Str(s) => Some(s.as_bytes()),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
            let b = crate::nextgen::arena_str_bytes(*p);
            std::str::from_utf8(b).ok().map(str::as_bytes)
        }
        _ => None,
    }
}

/// Natural order over a CLASSIFIED pair, without allocating; `None` for any pair this does
/// not classify (a heap object, an array, null, a mixed pair), which the caller sends to the
/// historic `natural_cmp`.
#[inline]
fn natural_hr(a: &Value, b: &Value) -> Option<i32> {
    match (a, b) {
        (Value::Int(_) | Value::Long(_), Value::Int(_) | Value::Long(_)) => {
            Some(sign(a.as_i64().cmp(&b.as_i64())))
        }
        (Value::Double(x), Value::Double(y)) => Some(java_double_compare(*x, *y)),
        (Value::Float(x), Value::Float(y)) => Some(java_float_compare(*x, *y)),
        _ => utf16_order(str_bytes(a)?, str_bytes(b)?),
    }
}

/// Natural order, fast where classified and the historic function where not — the same
/// answer `natural_cmp` gives for every pair it is asked.
#[inline]
fn nat(gate: u8, a: &Value, b: &Value) -> i32 {
    match natural_hr(a, b) {
        Some(r) => {
            hr_count(gate, C_NAT_FAST);
            r
        }
        None => match natural_tag_hr(a, b) {
            Some(r) => {
                cc_bump(CC_SERVED + SV_TAG);
                r
            }
            None => {
                hr_count(gate, C_NAT_SLOW);
                natural_cmp(a, b)
            }
        },
    }
}

// ═══════ CMP-CALLER: who calls `Vm::compare`, with what — and the lambda it calls, resolved once ═══════
//
// `Vm::compare` is the top host row under compiled bodies (~2.0%) AFTER the derive-once memo above
// measured neutral — so the cost is in how often it is called and by whom, not in its body. This
// block (kill switch `RAVA_NO_CMPCALLER=1`, or the blanket `RAVA_NO_EMIT_MORE`) does two things.
//
//   1. A CALLER CENSUS, always on, reported from the periodic profiler dump as `[CMP-CALLER]`
//      (appended to `hr_compare_report`, which `pc_profile/dump.rs` already prints): compares by
//      ENTRY POINT × ELEMENT KIND, by COMPARATOR KIND, and per entry the number of calls, elements
//      and compares — so N log N against N², and "the same collection re-sorted on every operation",
//      are visible as numbers. Per compare it is a few thread-local cell increments; the cells are
//      flushed into process counters every 4096 compares (a thread's last <4096 go unreported — a
//      census, not a ledger). Entry points are marked by a `CmpEntryScope` guard the callers create
//      (`Vm::cmp_entry`); a compare under no guard is "unmarked". Per-entry call statistics are
//      recorded ONCE per guard drop, straight into process counters (one event per sort, not per
//      compare); the per-compare `Comparator.compare` bridge records none.
//
//   2. THE LAMBDA CALL, RESOLVED PER COMPARATOR (PER SORT), NOT PER COMPARE. The historic K_LAMBDA
//      arm runs `invoke_lambda(h, vec![a, b])` on every comparison: a `Lambda` clone, a captures
//      clone, an `extend` that reallocates, a `LambdaSite` `Arc` bump, the apply-plan front probe,
//      `FastMap` memo probe in `flat_compile_first_use`, and only then `invoke_idx`. For a `REF_invokeStatic`
//      body (kind 6 — what javac emits for `(a, b) -> …` and `t -> t.key`) every one of those
//      answers is a constant of the SITE: `DirectLam` holds the resolved `(decl, midx)` once, and
//      each comparison builds the argument vector in a POOLED buffer and calls `invoke_idx` — the
//      same callee, the same arguments in the same order, rooted the same way (`push_call_roots`),
//      into the same compiled or interpreted body. Served: (a) the whole `sort_values` merge for a
//      lambda comparator (`sort_values_direct` — the same bottom-up stable merge schedule, through
//      `merge_sort_indices`, so the same pairs are compared in the same order); (b) the K_LAMBDA
//      memo arm; (c) both key-function applies of the K_KEY/K_KEYREV arm.
//
//   3. NATURAL ORDER OVER A PAIR OF OBJECT TAGS WITHOUT ALLOCATING. The historic `natural_cmp` orders
//      two heap objects by `display()` — `format!("<object@{h}>")` TWICE per comparison — and two
//      arena objects by `<arena-object@0x{addr:x}>`. `natural_tag_hr` renders the same bytes into a
//      stack buffer and compares them: the identical answer, no malloc. (That this order is a
//      HANDLE NUMBER and not `compareTo` is a pre-existing semantic gap, reported, not touched.)
//
// GUARD — what can only REJECT: any comparator or key function that is not a heap handle whose
// lambda header has kind 6, whose site owner resolves to bytecode (`resolve_method` `Some`,
// `method_index` `Some`, a body with code), whose owner is not forced native and is not the
// `$Lambda` pseudo-owner; any run with `RAVA_NO_FAST_INDYFAM`, `RAVA_LAMBDA_TRACE`,
// `RAVA_LAMBDA_STAT` or `trace.lambda` armed (their diagnostics would miss these calls). Everything
// refused takes the historic arm, verbatim. Each resolved lambda body is submitted to the fixed
// first-use compiler policy once per `Code`, not after a site/body invocation counter crosses a
// hotness threshold.
//
// NO ADAPT: no class, method or package NAME decides anything here; the predicate is the
// MethodHandle kind, the resolved dispatch and the presence of code.

/// `RAVA_NO_CMPCALLER` / `RAVA_NO_EMIT_MORE`, cached per thread in one byte (0 = unasked, 1 = on,
/// 2 = off) — the `INDYFAM_GATE` shape: a `Cell<u8>` with a `const` initialiser lowers to one TLS
/// load, where an acquire `OnceLock` read per compare is an optimisation barrier.
std::thread_local! {
    static CC_GATE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[inline(always)]
fn cc_on() -> bool {
    match CC_GATE.with(|c| c.get()) {
        1 => true,
        2 => false,
        _ => cc_cold(),
    }
}

#[cold]
#[inline(never)]
fn cc_cold() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| {
        // MEASURED OPT-IN on the bootstrap bench (3 pairs, B = disabled): +2.70/+3.32/+2.64% min but
        // -0.71/-0.53/-0.65% median -- the two statistics disagree in sign, which is the floor, not
        // a result; the census (`[CMP-CALLER]`) is what the round produced. `RAVA_FAST_CMPCALLER=1`.
        std::env::var_os("RAVA_FAST_CMPCALLER").is_some() && !crate::flags::emit_more_disabled()
    });
    CC_GATE.with(|c| c.set(if on { 1 } else { 2 }));
    on
}

// ── entry points (rows of the census; `Vm::CMP_EP_*` are the callers' names for them) ──
const EP_COUNT: usize = 12;
const EP_NAMES: [&str; EP_COUNT] = [
    "unmarked", "List.sort", "Collections.sort", "Arrays.sort", "Stream.sorted",
    "sorted-set order", "Comparator.compare bridge", "Stream.min/max", "Collections.min/max",
    "Collectors.minBy/maxBy", "Collections.binarySearch", "NavigableSet floor/ceiling",
];

// ── element kinds (columns) ──
const EK_STR: u8 = 0; // handle or arena string
const EK_NUM: u8 = 1; // int/long/float/double
const EK_HOBJ: u8 = 2; // heap-handle object
const EK_AOBJ: u8 = 3; // arena object (a guest object: enum constants, records, …)
const EK_NULL: u8 = 4;
const EK_ARRAY: u8 = 5;
const EK_MIXED: u8 = 6; // the two operands are of different kinds
const EK_OTHER: u8 = 7;
const EK_COUNT: usize = 8;
const EK_NAMES: [&str; EK_COUNT] = ["str", "num", "hobj", "aobj", "null", "array", "mixed", "other"];

// ── comparator kinds ──
const CK_NULL: u8 = 0; // no comparator: natural order
const CK_LAMBDA: u8 = 1; // heap-handle lambda
const CK_ALAMBDA: u8 = 2; // arena lambda (a capturing closure)
const CK_AOBJ: u8 = 3; // arena NON-lambda object — a guest Comparator; compare() is never called
const CK_SYNTH: u8 = 4; // `$cmp`-tagged combinator (reverse/key/then/nulls*/entry*)
const CK_UNTAGGED: u8 = 5; // heap handle with no `$cmp` — a guest Comparator; compare() never called
const CK_NATURAL: u8 = 6; // `Comparator.naturalOrder()`
const CK_DECLINE: u8 = 7; // memo refused the shape (ciorder, non-handle sub-comparator)
const CK_COUNT: usize = 8;
const CK_NAMES: [&str; CK_COUNT] = [
    "null(natural)", "lambda", "arena-lambda", "arena-object(NATURAL, compare() never called)",
    "synthetic", "untagged-handle(NATURAL, compare() never called)", "naturalOrder", "declined",
];

// ── the thread-local cells and their process-wide sums ──
const CC_EPK: usize = 0;
const CC_CK: usize = CC_EPK + EP_COUNT * EK_COUNT;
const CC_SERVED: usize = CC_CK + CK_COUNT;
const SV_SORT: usize = 0; // compares served by `sort_values_direct`
const SV_CMP: usize = 1; // K_LAMBDA memo arm served directly
const SV_KEY: usize = 2; // key-function applies served directly (two per K_KEY compare)
const SV_TAG: usize = 3; // natural-order object-tag pairs ordered without allocating
const SV_COUNT: usize = 4;
const CC_CELLS: usize = CC_SERVED + SV_COUNT;
const CC_FLUSH_EVERY: u64 = 4096;

std::thread_local! {
    /// The active entry point (a `Vm::CMP_EP_*`), set and restored by `CmpEntryScope`.
    static CC_EP: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// Compares seen on this thread — the guard reads it at entry and at drop.
    static CC_N: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Compares since the last flush.
    static CC_PENDING: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static CC_T: [std::cell::Cell<u32>; CC_CELLS] =
        const { [const { std::cell::Cell::new(0) }; CC_CELLS] };
}
static CC_G: [std::sync::atomic::AtomicU64; CC_CELLS] =
    [const { std::sync::atomic::AtomicU64::new(0) }; CC_CELLS];

/// Per-entry call statistics, one event per `CmpEntryScope` drop: calls, elements, compares, then
/// a five-bucket histogram of the element count (<8, <64, <512, <4096, ≥4096).
const ST_W: usize = 8;
static CC_ST: [std::sync::atomic::AtomicU64; EP_COUNT * ST_W] =
    [const { std::sync::atomic::AtomicU64::new(0) }; EP_COUNT * ST_W];

#[inline]
fn ek_of(v: &Value) -> u8 {
    match v {
        Value::Str(_) => EK_STR,
        Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_) => EK_NUM,
        Value::Ref(_) => EK_HOBJ,
        Value::Array(_) => EK_ARRAY,
        Value::Null => EK_NULL,
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            if crate::nextgen::is_arena_string(*p) { EK_STR } else { EK_AOBJ }
        }
        _ => EK_OTHER,
    }
}

#[inline]
fn ek_pair(a: &Value, b: &Value) -> u8 {
    let (x, y) = (ek_of(a), ek_of(b));
    if x == y { x } else { EK_MIXED }
}

#[inline]
fn cc_bump(i: usize) {
    CC_T.with(|t| t[i].set(t[i].get().wrapping_add(1)));
}

/// One compare, seen: entry × element kind, comparator kind, and the flush cadence.
#[inline]
fn cc_note(ck: u8, a: &Value, b: &Value) {
    let ep = CC_EP.with(|c| c.get()) as usize;
    let ek = ek_pair(a, b) as usize;
    CC_T.with(|t| {
        let i = CC_EPK + ep * EK_COUNT + ek;
        t[i].set(t[i].get().wrapping_add(1));
        let j = CC_CK + ck as usize;
        t[j].set(t[j].get().wrapping_add(1));
    });
    CC_N.with(|c| c.set(c.get() + 1));
    let p = CC_PENDING.with(|c| {
        let p = c.get() + 1;
        c.set(p);
        p
    });
    if p >= CC_FLUSH_EVERY {
        cc_flush();
    }
}

#[cold]
#[inline(never)]
fn cc_flush() {
    use std::sync::atomic::Ordering::Relaxed;
    CC_PENDING.with(|c| c.set(0));
    CC_T.with(|t| {
        for (i, c) in t.iter().enumerate() {
            let v = c.replace(0);
            if v != 0 {
                CC_G[i].fetch_add(v as u64, Relaxed);
            }
        }
    });
}

/// The comparator kind a memo slot stands for, for the census.
#[inline]
fn ck_of_memo(m: &CmpMemo) -> u8 {
    match m.kind {
        K_LAMBDA => CK_LAMBDA,
        K_DECLINE => CK_DECLINE,
        K_NATURAL => if m.flags & F_UNTAGGED != 0 { CK_UNTAGGED } else { CK_NATURAL },
        _ => CK_SYNTH,
    }
}

/// An entry-point mark: sets the thread's active entry for the guard's lifetime and, at drop,
/// records one call event for the entry (calls, elements, compares, size bucket). No borrow of
/// the `Vm` — the guard lives across `vm.compare` calls.
pub(crate) struct CmpEntryScope {
    prev: u8,
    start: u64,
    n: u32,
    ep: u8,
    armed: bool,
}

impl Drop for CmpEntryScope {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        CC_EP.with(|c| c.set(self.prev));
        // The bridge is one compare per guard: a call event per compare would be the
        // per-call atomic RMW this tree forbids, and its "calls" would equal its compares.
        if self.ep == Vm::CMP_EP_BRIDGE {
            return;
        }
        use std::sync::atomic::Ordering::Relaxed;
        let d = CC_N.with(|c| c.get()).saturating_sub(self.start);
        let n = self.n as u64;
        let b = ST_W * self.ep as usize;
        CC_ST[b].fetch_add(1, Relaxed);
        CC_ST[b + 1].fetch_add(n, Relaxed);
        CC_ST[b + 2].fetch_add(d, Relaxed);
        let bucket = match n { 0..=7 => 0, 8..=63 => 1, 64..=511 => 2, 512..=4095 => 3, _ => 4 };
        CC_ST[b + 3 + bucket].fetch_add(1, Relaxed);
    }
}

/// `[CMP-CALLER]` — the census, for the periodic dump. Empty when nothing was flushed.
fn cmp_caller_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let g = |i: usize| CC_G[i].load(Relaxed);
    let st = |ep: usize, k: usize| CC_ST[ep * ST_W + k].load(Relaxed);
    let total: u64 = (0..EP_COUNT * EK_COUNT).map(|i| g(CC_EPK + i)).sum();
    let calls: u64 = (0..EP_COUNT).map(|e| st(e, 0)).sum();
    if total + calls == 0 {
        return String::new();
    }
    let mut out = format!(
        "[CMP-CALLER] compares={total} (flushed per 4096/thread) | served directly: sort-merge={} memo-lambda={} key-fn applies={} | object-tag natural pairs without display()={}",
        g(CC_SERVED + SV_SORT), g(CC_SERVED + SV_CMP), g(CC_SERVED + SV_KEY), g(CC_SERVED + SV_TAG)
    );
    out.push_str(&format!("\n[CMP-CALLER]   {:<28}{:>10}", "entry \\ element kind", "total"));
    for name in EK_NAMES {
        out.push_str(&format!("{name:>9}"));
    }
    let mut rows: Vec<(u64, usize)> = (0..EP_COUNT)
        .map(|e| ((0..EK_COUNT).map(|k| g(CC_EPK + e * EK_COUNT + k)).sum::<u64>(), e))
        .filter(|r| r.0 > 0)
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    for (tot, e) in rows {
        out.push_str(&format!("\n[CMP-CALLER]   {:<28}{tot:>10}", EP_NAMES[e]));
        for k in 0..EK_COUNT {
            out.push_str(&format!("{:>9}", g(CC_EPK + e * EK_COUNT + k)));
        }
    }
    let mut cks: Vec<(u64, usize)> =
        (0..CK_COUNT).map(|k| (g(CC_CK + k), k)).filter(|r| r.0 > 0).collect();
    cks.sort_by(|a, b| b.0.cmp(&a.0));
    for (c, k) in cks {
        out.push_str(&format!("\n[CMP-CALLER]   comparator {c:>10}  {}", CK_NAMES[k]));
    }
    let mut eps: Vec<usize> = (0..EP_COUNT).filter(|&e| st(e, 0) > 0).collect();
    eps.sort_by(|a, b| st(*b, 2).cmp(&st(*a, 2)));
    for e in eps {
        let (c, n, d) = (st(e, 0), st(e, 1), st(e, 2));
        out.push_str(&format!(
            "\n[CMP-CALLER]   {:<28} calls={c:<8} elems={n:<10} compares={d:<10} compares/call={:<8.1} n<8:{} <64:{} <512:{} <4096:{} >=4096:{}",
            EP_NAMES[e],
            d as f64 / c.max(1) as f64,
            st(e, 3), st(e, 4), st(e, 5), st(e, 6), st(e, 7)
        ));
    }
    out
}

// ── natural order over object TAGS, without allocating (block point 3) ──

/// `{prefix}{v}>` into `buf`, decimal or lowercase hex — the bytes `display()` formats for a
/// handle object, an array or an arena object. Returns the length written.
fn tag_render(buf: &mut [u8; 40], prefix: &[u8], mut v: u64, hex: bool) -> usize {
    buf[..prefix.len()].copy_from_slice(prefix);
    let base: u64 = if hex { 16 } else { 10 };
    let mut digits = [0u8; 20];
    let mut n = 0usize;
    loop {
        let d = (v % base) as u8;
        digits[n] = if d < 10 { b'0' + d } else { b'a' + (d - 10) };
        n += 1;
        v /= base;
        if v == 0 {
            break;
        }
    }
    let mut len = prefix.len();
    for i in (0..n).rev() {
        buf[len] = digits[i];
        len += 1;
    }
    buf[len] = b'>';
    len + 1
}

/// The historic `natural_cmp` answer for two objects of the SAME representation — a byte compare
/// of their `display()` tags — computed on the stack. `None` for every other pair.
#[inline]
fn natural_tag_hr(a: &Value, b: &Value) -> Option<i32> {
    if !cc_on() {
        return None;
    }
    let (mut x, mut y) = ([0u8; 40], [0u8; 40]);
    let (lx, ly) = match (a, b) {
        (Value::Null, Value::Null) => return Some(0),
        (Value::Ref(p), Value::Ref(q)) => (
            tag_render(&mut x, b"<object@", *p as u64, false),
            tag_render(&mut y, b"<object@", *q as u64, false),
        ),
        (Value::Array(p), Value::Array(q)) => (
            tag_render(&mut x, b"<array@", *p as u64, false),
            tag_render(&mut y, b"<array@", *q as u64, false),
        ),
        #[cfg(feature = "next-gen-gc")]
        (Value::Obj(p), Value::Obj(q))
            if !crate::nextgen::is_arena_string(*p) && !crate::nextgen::is_arena_string(*q) =>
        (
            tag_render(&mut x, b"<arena-object@0x", p.addr(), true),
            tag_render(&mut y, b"<arena-object@0x", q.addr(), true),
        ),
        _ => return None,
    };
    Some(sign(x[..lx].cmp(&y[..ly])))
}

// ── the lambda implementation, resolved once per site (block point 2) ──

/// One kind-6 lambda implementation, resolved once. Shared (`Rc`) by every comparator and key
/// function minted at the same `invokedynamic` site — the resolution is a constant of the site.
struct DirectLam {
    site: Arc<crate::heap::LambdaSite>,
    decl: Arc<crate::class_loader::LoadedClass>,
    midx: u32,
    /// Whether this site's resolved body has received its fixed first-use compile request.
    compile_requested: std::cell::Cell<bool>,
}

/// The first resolved lambda invocation requests compilation through the normal structural JIT
/// gates. This memo avoids repeated requests; it does not count calls or select a hotness tier.
const NO_DIRECT: u32 = u32::MAX;
const DIRECT_CAP: usize = 1024;

std::thread_local! {
    /// Append-only, so an index handed to a memo slot stays valid for the thread's life. Bounded
    /// by the number of distinct comparator/key-function sites this thread sorts through.
    static DIRECT: std::cell::RefCell<Vec<std::rc::Rc<DirectLam>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[inline]
fn direct_get(x: u32) -> Option<std::rc::Rc<DirectLam>> {
    if x == NO_DIRECT {
        return None;
    }
    DIRECT.with(|t| t.borrow().get(x as usize).cloned())
}

/// The outcome of a direct call: refused (nothing ran, nothing changed — the caller takes the
/// historic arm) or ran, with the body's return.
enum Direct {
    Refused,
    Ran(Option<Value>),
}

impl Vm {
    /// Compare two values with a Comparator (a lambda, or a synthetic object
    /// built by `Comparator.comparing`/`naturalOrder`/`reversed`/`thenComparing`).
    pub(crate) fn compare(&mut self, cmp: &Value, a: &Value, b: &Value) -> Result<i32, VmError> {
        // ⭐**ONE NORMALISATION AT THE ENTRY PUTS THE WHOLE COMPARISON SUBSYSTEM BACK ON THE
        // OPERAND SHAPE IT WAS WRITTEN FOR.** Everything below — `compare_hr`, `nat`,
        // `natural_hr`, `natural_tag_hr`, `scratch::natural_cmp`, and the eight recursive
        // `natural_cmp` calls in the comparator-kind arms — decides from a `&Value` with no
        // access to the heap, so none of them can unbox the `Character`/`Short`/`Byte` objects
        // `crate::boxid` now mints, and `natural_cmp`'s fallback compared their HANDLE
        // RENDERINGS. See `Vm::unbox_narrow` for what that cost.
        //
        // Unboxing HERE rather than in each helper is also what makes it safe: a real guest
        // comparator reached below is invoked with these same operands, which is exactly the
        // `Value::Int` it received before this work — and a `Character`-typed lambda body
        // unboxes it with `charValue()`, whose arm reads a raw `Value::Int` unchanged. So every
        // path sees the shape it saw before, and nothing sees a half-converted pair.
        //
        // Costs a `class_of` per `Value::Ref` OPERAND, behind a `boxid::enabled()` bool and a
        // tag test — nothing at all for the primitive and string pairs that are the bulk of the
        // traffic, and nothing when the kill switch is set.
        let (norm_a, norm_b) = (self.unbox_narrow(a), self.unbox_narrow(b));
        let a = norm_a.as_ref().unwrap_or(a);
        let b = norm_b.as_ref().unwrap_or(b);
        let gate = hr_gate();
        if gate & HR_ON != 0 {
            if let Some(r) = self.compare_hr(gate, cmp, a, b)? {
                return Ok(r);
            }
        }
        // RAVA_ARENA_LAMBDA: a capturing comparator lambda is an arena Value::Obj.
        #[cfg(feature = "next-gen-gc")]
        if let Value::Obj(p) = cmp {
            if crate::nextgen::is_arena_lambda(*p) {
                return Ok(self
                    .invoke_lambda_value(cmp, vec![a.clone(), b.clone()])?
                    .and_then(|v| v.as_int())
                    .unwrap_or(0));
            }
        }
        let h = match cmp {
            Value::Ref(h) => *h,
            _ => return Ok(natural_cmp(a, b)),
        };
        if self.heap.is_lambda(h) {
            return Ok(self
                .invoke_lambda(h, vec![a.clone(), b.clone()])?
                .and_then(|v| v.as_int())
                .unwrap_or(0));
        }
        // ⭐**THE COMPARATOR KIND IS ONLY MATCHED AGAINST LITERALS, SO IT IS NO LONGER COPIED.**
        // `get_field` hands back an owned `Value` that already holds the interned `RStr`; the
        // `to_string()` that stood here allocated a private copy of a seven-byte tag purely to
        // feed a `match` on `&str`. Was ONE heap allocation + copy per COMPARISON — and the
        // quantity that grows is comparisons, i.e. `n log n` per sort, with `reversed`/`then`/
        // `nullsFirst` recursing into this function and allocating again at every level. Now a
        // borrow out of a local that does not borrow `self`, so the `&mut self` calls in the
        // arms below (`apply`, the recursive `compare`) are unaffected. A missing or non-string
        // `$cmp` still reads as `""` and still leaves through the same `_` arm.
        let kind_v = self.heap.get_field(h, "$cmp");
        let kind: &str = match &kind_v {
            Value::Str(s) => s.as_str(),
            _ => "",
        };
        match kind {
            "reverse" => Ok(-natural_cmp(a, b)),
            // `String.CASE_INSENSITIVE_ORDER`: compare the folded forms, and fall back to
            // the natural order for anything that is not a pair of strings.
            "ciorder" => Ok(match (
                str_bytes(a).and_then(|bytes| std::str::from_utf8(bytes).ok()),
                str_bytes(b).and_then(|bytes| std::str::from_utf8(bytes).ok()),
            ) {
                (Some(x), Some(y)) => {
                    let (lx, ly) = (x.to_lowercase(), y.to_lowercase());
                    match lx.cmp(&ly) {
                        std::cmp::Ordering::Less => -1,
                        std::cmp::Ordering::Equal => 0,
                        std::cmp::Ordering::Greater => 1,
                    }
                }
                _ => natural_cmp(a, b),
            }),
            "key" | "keyrev" => {
                let kf = self.heap.get_field(h, "$keyfn");
                let ka = self.apply(&kf, vec![a.clone()])?.unwrap_or(Value::Null);
                let kb = self.apply(&kf, vec![b.clone()])?.unwrap_or(Value::Null);
                let r = natural_cmp(&ka, &kb);
                Ok(if kind == "keyrev" { -r } else { r })
            }
            // Map.Entry.comparingByKey/Value: compare two entries by $key/$val.
            "entrykey" | "entryval" => {
                let f = if kind == "entryval" { "$val" } else { "$key" };
                let ea = match a {
                    Value::Ref(h) => self.heap.get_field(*h, f),
                    _ => Value::Null,
                };
                let eb = match b {
                    Value::Ref(h) => self.heap.get_field(*h, f),
                    _ => Value::Null,
                };
                Ok(natural_cmp(&ea, &eb))
            }
            // NULLS ORDERED FIRST OR LAST, everything else delegated. Two nulls are
            // equal; a null against a non-null is decided by the wrapper alone and the
            // inner comparator is NEVER consulted with a null operand — which is the
            // whole point of the wrapper, since the inner one is free to throw on one.
            "nullsfirst" | "nullslast" => {
                let first = kind == "nullsfirst";
                let an = matches!(a, Value::Null);
                let bn = matches!(b, Value::Null);
                if an || bn {
                    return Ok(match (an, bn) {
                        (true, true) => 0,
                        (true, false) => if first { -1 } else { 1 },
                        _ => if first { 1 } else { -1 },
                    });
                }
                // `nullsFirst(null)`: the JDK's wrapper considers all non-nulls equal.
                if self.heap.get_field(h, "$nocmp").as_int() == Some(1) {
                    return Ok(0);
                }
                let inner = self.heap.get_field(h, "$inner");
                self.compare(&inner, a, b)
            }
            "reversed" => {
                let inner = self.heap.get_field(h, "$inner");
                Ok(-self.compare(&inner, a, b)?)
            }
            "then" => {
                let first = self.heap.get_field(h, "$first");
                let r = self.compare(&first, a, b)?;
                if r != 0 {
                    Ok(r)
                } else {
                    let second = self.heap.get_field(h, "$second");
                    self.compare(&second, a, b)
                }
            }
            _ => Ok(natural_cmp(a, b)), // naturalOrder / unknown
        }
    }

    /// HR-COMPARE fast arm (see the block comment above `hr_gate`). `Ok(None)` is a refusal:
    /// nothing has run and nothing has changed, and the historic body of `compare` takes the
    /// same three arguments from the top.
    #[inline]
    fn compare_hr(
        &mut self,
        gate: u8,
        cmp: &Value,
        a: &Value,
        b: &Value,
    ) -> Result<Option<i32>, VmError> {
        let h = match cmp {
            Value::Ref(h) => *h,
            // An arena comparator (arena lambda or arena object) moves under a collection and
            // is not a handle; the historic arm owns it.
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => {
                hr_count(gate, C_ARENA_CMP);
                if cc_on() {
                    let ck = if crate::nextgen::is_arena_lambda(*p) { CK_ALAMBDA } else { CK_AOBJ };
                    cc_note(ck, a, b);
                }
                return Ok(None);
            }
            // Null / anything else: the historic arm is `natural_cmp(a, b)`.
            _ => {
                hr_count(gate, C_BASE + K_NATURAL as usize);
                if cc_on() {
                    cc_note(CK_NULL, a, b);
                }
                return Ok(Some(nat(gate, a, b)));
            }
        };
        let epoch = crate::heap::handle_epoch();
        let slot = memo_slot(h);
        let mut m = CMP_MEMO.with(|t| t[slot].get());
        let hit = m.kind != K_NONE
            && m.handle == h
            && m.epoch == epoch
            && object_addr(&self.heap, h) == Some(m.addr);
        if hit {
            hr_count(gate, C_HIT);
        } else {
            hr_count(gate, C_MISS);
            m = match self.derive_cmp(h, epoch) {
                Some(m) => m,
                // No object behind the handle: nothing to remember, historic arm.
                None => return Ok(None),
            };
            CMP_MEMO.with(|t| t[slot].set(m));
        }
        hr_count(gate, C_BASE + m.kind as usize);
        if cc_on() {
            cc_note(ck_of_memo(&m), a, b);
        }
        Ok(Some(match m.kind {
            K_DECLINE => {
                hr_count(gate, C_DECLINED);
                return Ok(None);
            }
            K_LAMBDA => {
                // Resolved once (`m.x`, see `derive_cmp`): the same body, called through
                // `invoke_idx` with a pooled argument vector instead of the general apply.
                if let Some(dl) = direct_get(m.x) {
                    if let Direct::Ran(v) = self.call_direct(&dl, h, a.clone(), Some(b.clone()))? {
                        cc_bump(CC_SERVED + SV_CMP);
                        return Ok(Some(v.and_then(|v| v.as_int()).unwrap_or(0)));
                    }
                }
                self.invoke_lambda(h, vec![a.clone(), b.clone()])?
                    .and_then(|v| v.as_int())
                    .unwrap_or(0)
            }
            K_NATURAL => nat(gate, a, b),
            K_REVERSE => -nat(gate, a, b),
            K_KEY | K_KEYREV => {
                let kf = sub_value(m.x);
                // The key function, resolved once (`m.y`): `apply` on a handle lambda IS
                // `invoke_lambda`, so this is the same body with the same one argument.
                let ka = self.apply_key(m.y, m.x, &kf, a)?;
                let kb = self.apply_key(m.y, m.x, &kf, b)?;
                let r = nat(gate, &ka, &kb);
                if m.kind == K_KEYREV { -r } else { r }
            }
            K_ENTRYKEY | K_ENTRYVAL => {
                let f = if m.kind == K_ENTRYVAL { crate::heap::Syn::Val } else { crate::heap::Syn::Key };
                let ea = match a {
                    Value::Ref(h) => self.heap.get_field_syn(*h, f),
                    _ => Value::Null,
                };
                let eb = match b {
                    Value::Ref(h) => self.heap.get_field_syn(*h, f),
                    _ => Value::Null,
                };
                nat(gate, &ea, &eb)
            }
            K_NULLSFIRST | K_NULLSLAST => {
                let first = m.kind == K_NULLSFIRST;
                let an = matches!(a, Value::Null);
                let bn = matches!(b, Value::Null);
                if an || bn {
                    match (an, bn) {
                        (true, true) => 0,
                        (true, false) => if first { -1 } else { 1 },
                        _ => if first { 1 } else { -1 },
                    }
                } else if m.flags & F_NOCMP != 0 {
                    0
                } else {
                    self.compare(&sub_value(m.x), a, b)?
                }
            }
            K_REVERSED => -self.compare(&sub_value(m.x), a, b)?,
            K_THEN => {
                let r = self.compare(&sub_value(m.x), a, b)?;
                if r != 0 {
                    r
                } else if crate::heap::handle_epoch() != epoch {
                    // The first clause ran guest code and a collection retired slots since the
                    // memo was stamped: take `$second` from the object, exactly as the historic
                    // arm does, rather than from a handle that may have been re-issued.
                    let second = self.heap.get_field(h, "$second");
                    self.compare(&second, a, b)?
                } else {
                    self.compare(&sub_value(m.y), a, b)?
                }
            }
            _ => return Ok(None),
        }))
    }

    /// Classify the comparator behind `h` — the historic dispatch of `compare`, performed once.
    /// Reads only; runs no guest code. `None` when there is no object at `h`.
    fn derive_cmp(&self, h: u32, epoch: u64) -> Option<CmpMemo> {
        let addr = object_addr(&self.heap, h)?;
        let mut m = CmpMemo { handle: h, kind: K_DECLINE, flags: 0, x: 0, y: 0, epoch, addr };
        if self.heap.is_lambda(h) {
            m.kind = K_LAMBDA;
            m.x = self.direct_lam_for(h);
            return Some(m);
        }
        let kind_v = self.heap.get_field(h, "$cmp");
        let kind: &str = match &kind_v {
            Value::Str(s) => s.as_str(),
            _ => {
                // No `$cmp` tag at all: a guest object handed in as a Comparator. The historic
                // arm orders by `natural_cmp` and never calls its `compare` — flagged for the
                // census so that population has a number.
                m.flags |= F_UNTAGGED;
                ""
            }
        };
        // Every shape below mirrors an arm of the historic `match`; a sub-comparator or key
        // function that is neither a handle nor null leaves the slot as `K_DECLINE`.
        match kind {
            "reverse" => m.kind = K_REVERSE,
            "ciorder" => {}
            "key" | "keyrev" => {
                if let Some(x) = sub_of(&self.heap.get_field(h, "$keyfn")) {
                    m.x = x;
                    m.y = if x != SUB_NULL && self.heap.is_lambda(x) {
                        self.direct_lam_for(x)
                    } else {
                        NO_DIRECT
                    };
                    m.kind = if kind == "keyrev" { K_KEYREV } else { K_KEY };
                }
            }
            "entrykey" => m.kind = K_ENTRYKEY,
            "entryval" => m.kind = K_ENTRYVAL,
            "nullsfirst" | "nullslast" => {
                let k = if kind == "nullsfirst" { K_NULLSFIRST } else { K_NULLSLAST };
                if self.heap.get_field(h, "$nocmp").as_int() == Some(1) {
                    m.flags |= F_NOCMP;
                    m.kind = k;
                } else if let Some(x) = sub_of(&self.heap.get_field(h, "$inner")) {
                    m.x = x;
                    m.kind = k;
                }
            }
            "reversed" => {
                if let Some(x) = sub_of(&self.heap.get_field(h, "$inner")) {
                    m.x = x;
                    m.kind = K_REVERSED;
                }
            }
            "then" => {
                let first = sub_of(&self.heap.get_field(h, "$first"));
                let second = sub_of(&self.heap.get_field(h, "$second"));
                if let (Some(x), Some(y)) = (first, second) {
                    m.x = x;
                    m.y = y;
                    m.kind = K_THEN;
                }
            }
            _ => m.kind = K_NATURAL, // naturalOrder / unknown
        }
        Some(m)
    }

    /// Drain a jar iterator that was born as an ARENA object (`Value::Obj`) —
    /// see the call site in `iterate_object`. Same hasNext()/next() bytecode as
    /// the handle path, with one extra obligation: an arena receiver MOVES when a
    /// callee collects, so the iterator itself is rooted in `gc_protect` and
    /// re-read (forwarded) before every call, exactly like the receiver rooting in
    /// `invoke_ctor_ref`. Drained elements are rooted above it and split off at the
    /// end, so the iterator root is dropped and never leaks into the result.
    ///
    /// ⛔**AND IT STAYS A `Vec` — THE LAZY VERSION IS REFUSED, ON SEMANTICS, NOT ON EFFORT.**
    /// Draining eagerly is O(size) for a `for` loop that may `break` on the first element, and
    /// the obvious cure is to hand back a cursor that pulls `hasNext`/`next` as the guest steps.
    /// It cannot be done here without changing what the guest observes, in three ways at once:
    ///   * **Fail-fast moves.** Today the drain completes BEFORE any loop body runs, so the guest
    ///     iterates a private snapshot and a body that mutates the collection is reported by the
    ///     length check the CALLER does (`stream::dispatch`, `coll_op`). Pulled lazily, the jar's
    ///     own iterator sees the mutation and throws `ConcurrentModificationException` from a
    ///     different place, at a different element, and in cases where today's snapshot throws
    ///     nothing at all. That is a change in observable exception behaviour, not in speed.
    ///   * **The element sequence interleaves.** `next()` is guest bytecode with arbitrary side
    ///     effects; running it between loop bodies rather than all up front reorders every effect
    ///     it has against every effect the body has.
    ///   * **The caller contract is a `Vec`.** `iterate_object`/`iterate_recv` return one to ~70
    ///     readers that index, sort, re-order (`order_sorted_set`) and re-encode it; a cursor
    ///     would have to be materialised at almost all of them anyway.
    /// The precondition for laziness — "no observer can tell" — is not provable here, so this
    /// refuses and falls through to the drain it always did. What IS taken out below is work the
    /// eager drain was doing and discarding: the second hierarchy walk.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn drain_arena_iterator(&mut self, it_val: Value) -> Result<Vec<Value>, VmError> {
        // ⭐**THE NAME IS ONLY COMPARED, SO IT IS NO LONGER COPIED.** `class_name` already hands
        // back the arena registry's own `Arc<str>`; the `to_string()` that stood here allocated a
        // private copy of a class name to feed two `&str` parameters and then dropped it. Was one
        // `String` allocation + copy per DRAIN — a quantity that grows with collections drained,
        // which registry building and codec decode do constantly — and is now a borrow.
        let icls_arc = match &it_val {
            Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)),
            _ => None,
        };
        let icls: &str = icls_arc.as_deref().unwrap_or("");
        // ⭐**A QUESTION WHOSE ANSWER IS DISCARDED IS STILL PAID FOR.** The tuple here evaluated
        // BOTH resolves before the match could reject on the first, and a `hasNext` miss has
        // already walked — and loaded — the whole superclass-and-interface hierarchy, so the
        // `next` lookup could only re-walk it to reach the same refusal. Was two resolutions per
        // drain of every iterator without drainable bytecode (a `tlcache` probe each on a hit, a
        // full `super_chain` walk each on a miss); is one. The drainable path is unchanged — it
        // still asks both, in the same order.
        let Some(hn) = self.resolve_method(icls, "hasNext", "()Z")? else {
            return Ok(Vec::new()); // iterator without bytecode — can't drain
        };
        let Some(nx) = self.resolve_method(icls, "next", "()Ljava/lang/Object;")? else {
            return Ok(Vec::new());
        };
        // The callee slots are a constant of THIS iterator — hoisted out of the loop; see
        // `iter_midx` for what each element used to pay to re-derive them.
        let (hni, nxi) = (
            iter_midx(&hn, "hasNext", "()Z"),
            iter_midx(&nx, "next", "()Ljava/lang/Object;"),
        );
        let ibase = crate::vm::thread_context::root_len(&self.gc_protect);
        crate::vm::thread_context::push_root(&mut self.gc_protect, it_val);
        let base = crate::vm::thread_context::root_len(&self.gc_protect);
        loop {
            let recv =
                crate::vm::thread_context::root_index(&self.gc_protect, ibase); // forwarded across the last call
            let has = self
                .invoke_idx(&hn, hni, "hasNext", "()Z", vec![recv])?
                .and_then(|v| v.as_int())
                .unwrap_or(0);
            if has == 0 {
                break;
            }
            let recv =
                crate::vm::thread_context::root_index(&self.gc_protect, ibase);
            let v = self
                .invoke_idx(&nx, nxi, "next", "()Ljava/lang/Object;", vec![recv])?
                .unwrap_or(Value::Null);
            crate::vm::thread_context::push_root(&mut self.gc_protect, v);
            if crate::vm::thread_context::root_len(&self.gc_protect) - base > 2_000_000 {
                break;
            }
        }
        let out = crate::vm::thread_context::split_off_roots(&mut self.gc_protect, base);
        crate::vm::thread_context::truncate_roots(&mut self.gc_protect, ibase); // drop the iterator root
        Ok(out)
    }

    /// Materialise a collection's elements. Our synthetic collections store
    /// `$elems`; a real (jar) collection (e.g. fastutil) is drained through its
    /// own iterator() bytecode so we don't read a non-existent `$elems`.
    pub(crate) fn iterate_object(&mut self, obj: u32) -> Result<Vec<Value>, VmError> {
        // A `subList` view resolves against its BACKING list first, so a read through the
        // view sees whatever the parent holds now rather than a snapshot taken at creation.
        // This also refreshes the view's own `$elems`, which is what the ~69 direct readers
        // of that field see (crate::view documents why the snapshot has to exist at all).
        if let Some(v) = crate::view::read(self, obj) {
            return Ok(v);
        }
        if let Value::Array(a) = self.heap.get_field_syn(obj, crate::heap::Syn::Elems) {
            let mut vals = self.heap.elems_values(obj, a);
            // Honour an explicit `$size` when the collection carries one — a guest-bytecode
            // class keeps spare capacity in `$elems` so its `add` can be amortised without a
            // native call. Absent `$size` this truncate is a no-op (`elems_len` answers the
            // array length), so every native-fabricated collection reads exactly as before.
            // A SORTED set's order is its contract, and THIS is the one place every
            // drain of an `$elems`-backed collection passes through — the copy
            // constructors, `stream::dispatch`'s early Iterable path and `coll_op`
            // alike. The rule lived only in `coll_op` before, so the same `TreeSet`
            // printed sorted through `toString` and iterated in INSERTION order
            // through a for-each. Per DRAIN, not per element: the cost is one
            // `class_of` and a short substring test on a path that is already
            // allocating a Vec.
            crate::stream::collections::coll_op::order_sorted_set(
                self, &Value::Ref(obj), &mut vals)?;
            return Ok(vals);
        }
        // A jar-internal Path is Iterable<Path> over its name elements (segments).
        // Joiner.join / for-each over a Path relies on this.
        if let Value::Str(entry) = self.heap.get_field(obj, "$jarres") {
            return Ok(entry
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| crate::nio::make_jar_path(self, s))
                .collect());
        }
        // A real-bytecode List whose iterator()/listIterator() delegate to each
        // other (e.g. guava views like Lists.reverse, whose iterator() calls
        // listIterator() — served generically since AbstractList is JDK — which
        // drains via iterator() again) would recurse forever through the
        // collection natives. Drain such lists by index instead, which goes
        // through their concrete get(int)/size() and never re-enters here.
        // ⭐**THE CLASS NAME IS ONLY COMPARED, SO IT IS NO LONGER COPIED.** `class_of` returns a
        // borrow of the object header's own `Arc<str>` and `resolve_method` takes `&self`, so the
        // two coexist inside a scope; the `to_string()` that stood here allocated a private copy
        // of a class name on EVERY drain of EVERY non-`$elems` collection. Was one `String`
        // allocation + copy per drain — the quantity that grows is COLLECTIONS DRAINED, which
        // registry building and codec decode do constantly — and is now a borrow. Re-reading
        // `class_of` for the `iterator()` resolve below is a bounds-checked index into the object
        // table, and an object's class cannot change under it.
        let (szm, getm) = {
            let cls = self.heap.class_of(obj).unwrap_or("");
            // Short-circuited for the reason spelled out over the `hasNext`/`next` pair in
            // `drain_arena_iterator`: `get(int)` is only consulted when `size()` resolved, and a
            // `size()` miss has already walked the whole hierarchy. Was two resolutions per drain
            // of every collection that does not expose `size()` — which is every jar collection
            // that reaches the `iterator()` route below; is one.
            match self.resolve_method(cls, "size", "()I")? {
                Some(sz) => (
                    Some(sz),
                    self.resolve_method(cls, "get", "(I)Ljava/lang/Object;")?,
                ),
                None => (None, None),
            }
        };
        if let (Some(sz), Some(getm)) = (szm, getm) {
            let n = self
                .invoke(sz, "size", "()I", vec![Value::Ref(obj)])?
                .and_then(|v| v.as_int())
                .unwrap_or(0);
            if (0..5_000_000).contains(&n) {
                // Constant of the drain, hoisted: every one of the `n` `get(i)` calls below used
                // to re-hash `"get"`+descriptor and clone/drop the declaring class `Arc`.
                let gi = iter_midx(&getm, "get", "(I)Ljava/lang/Object;");
                // next-gen-gc: root the drained arena values in gc_protect (forwarded
                // in place each collection a `get` invoke triggers); a plain local Vec
                // keeps stale pre-move addresses. See iterate_recv. Default: unchanged.
                // A GIL-free fork worker must NOT touch the shared `gc_protect` (concurrent
                // push/split_off corrupts it → "split index > len" panic). No GC runs inside a
                // fork region (workers abort on nursery-full, main is parked), so the arena-value
                // rooting is unnecessary here — drain into a plain local Vec.
                #[cfg(feature = "next-gen-gc")]
                if crate::parallel::gilfree() {
                    let mut out = Vec::with_capacity(n as usize);
                    for i in 0..n {
                        out.push(
                            self.invoke_idx(&getm, gi, "get", "(I)Ljava/lang/Object;", vec![Value::Ref(obj), Value::Int(i)])?
                                .unwrap_or(Value::Null),
                        );
                    }
                    return Ok(out);
                }
                #[cfg(feature = "next-gen-gc")]
                {
                    let base = crate::vm::thread_context::root_len(&self.gc_protect);
                    for i in 0..n {
                        let v = self
                            .invoke_idx(&getm, gi, "get", "(I)Ljava/lang/Object;", vec![Value::Ref(obj), Value::Int(i)])?
                            .unwrap_or(Value::Null);
                        crate::vm::thread_context::push_root(&mut self.gc_protect, v);
                    }
                    return Ok(crate::vm::thread_context::split_off_roots(
                        &mut self.gc_protect,
                        base,
                    ));
                }
                #[cfg(not(feature = "next-gen-gc"))]
                {
                    let mut out = Vec::with_capacity(n as usize);
                    for i in 0..n {
                        out.push(
                            self.invoke_idx(&getm, gi, "get", "(I)Ljava/lang/Object;", vec![Value::Ref(obj), Value::Int(i)])?
                                .unwrap_or(Value::Null),
                        );
                    }
                    return Ok(out);
                }
            }
        }
        // Otherwise drain via REAL iterator() bytecode (resolve, not native
        // fallback) to avoid recursing back into the collection natives.
        let iterm = {
            let cls = self.heap.class_of(obj).unwrap_or("");
            self.resolve_method(cls, "iterator", "()Ljava/util/Iterator;")?
        };
        let it_val = match iterm {
            Some(decl) => match self.invoke(
                decl,
                "iterator",
                "()Ljava/util/Iterator;",
                vec![Value::Ref(obj)],
            )? {
                Some(v) => v,
                None => return Ok(Vec::new()),
            },
            None => return Ok(Vec::new()),
        };
        // ARENA ITERATOR (RAVA_ARENA_BIRTH_NS_GUARD, default OFF): this drain only
        // ever accepted a HANDLE iterator, silently returning an EMPTY element list
        // for anything else. That held while the collection SUBSTRING heuristic
        // force-handled every `*Set*`/`*List*`-named class — including gson's
        // `LinkedTreeMap$EntrySet$1`, the iterator behind EVERY JSON object's
        // entrySet(). Once the birth namespace guard lets that class be born in the
        // arena, this returned no entries, so each datapack JSON object decoded as
        // empty and the vanilla world_preset failed with "Missing overworld
        // dimension". Drive the same hasNext()/next() bytecode on an arena receiver
        // instead. Flag OFF cannot produce a `Value::Obj` here, so the historic
        // handle-only path below is untouched and byte-identical.
        #[cfg(feature = "next-gen-gc")]
        if matches!(it_val, Value::Obj(_)) {
            return self.drain_arena_iterator(it_val);
        }
        let it = match it_val {
            Value::Ref(h) => h,
            _ => return Ok(Vec::new()),
        };
        // A native iterator (from iter_op/iterator_of) carries its elements in
        // $elems from $pos — drain it directly. The bytecode hasNext/next path
        // below only works for a jar iterator; a JDK/gson iterator (e.g. the one
        // JsonArray.iterator() yields) has no resolvable bytecode, which made
        // spliterator()/stream() on such objects empty (JsonOps.getIntStream → []
        // → "not a list of 3 ints" for every BlockPos/Vec3i in worldgen).
        if let Value::Array(arr) = self.heap.get_field_syn(it, crate::heap::Syn::Elems) {
            // Spare capacity is not elements — see `Heap::elems_values`.
            let mut all = self.heap.elems_values(it, arr);
            let pos = self.heap.get_field_syn(it, crate::heap::Syn::Pos).as_int().unwrap_or(0).max(0) as usize;
            // ⭐**`skip(pos).collect()` BUILT A SECOND `Vec` FOR THE WHOLE REMAINDER, EVERY TIME.**
            // `Iterator::skip` cannot hand its adapter's allocation back to `collect`, so the
            // decode's `Vec<Value>` was thrown away and every surviving element copied into a
            // fresh one — and `pos` is 0 for a fresh iterator, which is the common case, so that
            // allocation and O(elements) copy bought nothing at all. `Vec::drain` removes the
            // consumed prefix IN PLACE. Was one extra allocation + O(elements) copy per drain,
            // unconditionally; is zero allocations and O(elements - pos) bytes moved only when the
            // iterator has actually been stepped. Same elements, same order, same `Vec` — no
            // behaviour to switch, so no flag.
            if pos > 0 {
                all.drain(..pos.min(all.len()));
            }
            return Ok(all);
        }
        let (hn, nx) = {
            // Borrowed, not `to_string()`d — same reason as the receiver's name above.
            let icls = self.heap.class_of(it).unwrap_or("");
            // Short-circuited; see `drain_arena_iterator`.
            let Some(hn) = self.resolve_method(icls, "hasNext", "()Z")? else {
                return Ok(Vec::new()); // iterator without bytecode — can't drain
            };
            let Some(nx) = self.resolve_method(icls, "next", "()Ljava/lang/Object;")? else {
                return Ok(Vec::new());
            };
            (hn, nx)
        };
        // Constants of THIS iterator, hoisted out of the per-element loops below — see
        // `iter_midx` for what each `hasNext`/`next` step used to re-derive.
        let (hni, nxi) = (
            iter_midx(&hn, "hasNext", "()Z"),
            iter_midx(&nx, "next", "()Ljava/lang/Object;"),
        );
        // `it` is a stable heap handle, but the drained arena values must be rooted in
        // gc_protect across the next() invokes (each can move them). next-gen-gc only.
        #[cfg(feature = "next-gen-gc")]
        {
            let base = crate::vm::thread_context::root_len(&self.gc_protect);
            loop {
                let has = self
                    .invoke_idx(&hn, hni, "hasNext", "()Z", vec![Value::Ref(it)])?
                    .and_then(|v| v.as_int())
                    .unwrap_or(0);
                if has == 0 {
                    break;
                }
                let v = self
                    .invoke_idx(&nx, nxi, "next", "()Ljava/lang/Object;", vec![Value::Ref(it)])?
                    .unwrap_or(Value::Null);
                crate::vm::thread_context::push_root(&mut self.gc_protect, v);
                if crate::vm::thread_context::root_len(&self.gc_protect) - base > 2_000_000 {
                    break;
                }
            }
            return Ok(crate::vm::thread_context::split_off_roots(
                &mut self.gc_protect,
                base,
            ));
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            let mut out = Vec::new();
            for _ in 0..2_000_000 {
                let has = self
                    .invoke_idx(&hn, hni, "hasNext", "()Z", vec![Value::Ref(it)])?
                    .and_then(|v| v.as_int())
                    .unwrap_or(0);
                if has == 0 {
                    break;
                }
                out.push(
                    self.invoke_idx(&nx, nxi, "next", "()Ljava/lang/Object;", vec![Value::Ref(it)])?
                        .unwrap_or(Value::Null),
                );
            }
            Ok(out)
        }
    }

    /// Runtime class name of any object `Value` — heap handle or arena object.
    pub(crate) fn value_class_name(&self, v: &Value) -> String {
        match v {
            Value::Ref(h) => self.heap.class_of(*h).unwrap_or("").to_string(),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                .map(|s| s.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Drain ANY iterable receiver — heap handle OR arena object — to a Vec. A heap
    /// `Ref` keeps the `$elems`/`$jarres`/size+get/iterator fast paths via
    /// `iterate_object`. An arena `Value::Obj` has no synthetic heap fields, so drain
    /// it through its real `size()`/`get(int)` or `iterator()` bytecode (the plain
    /// `java.lang.Iterable`/`Collection` contract — NO class hardcode), invoking with
    /// the arena receiver directly. This is what lets a Minecraft registry's
    /// `spliterator()`/`stream()` (JDK `Iterable` defaults, no bytecode in the jar)
    /// resolve on an arena receiver instead of throwing `UnsatisfiedLinkError`.
    pub(crate) fn iterate_recv(&mut self, recv: &Value) -> Result<Vec<Value>, VmError> {
        if let Value::Ref(h) = recv {
            return self.iterate_object(*h);
        }
        // ⭐**ONE `String` PER DRAIN, FOR A NAME THAT IS ONLY COMPARED.** `value_class_name`
        // `to_string()`s the class name, and four other modules already carry notes about paying
        // for that at their own call sites. Here the receiver is always an ARENA object — a heap
        // `Ref` left through `iterate_object` two lines above — and `nextgen::class_name` hands
        // back the registry's own `Arc<str>`, so borrowing it yields the same bytes with no
        // allocation. Was one `String` allocation + copy per DRAIN (the quantity that grows is
        // collections drained); is a borrow. An unnamed class still reads as `""` and still
        // leaves through the same empty return.
        #[cfg(feature = "next-gen-gc")]
        let cls_arc = match recv {
            Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)),
            _ => None,
        };
        #[cfg(not(feature = "next-gen-gc"))]
        let cls_arc: Option<Arc<str>> = None;
        let cls: &str = cls_arc.as_deref().unwrap_or("");
        if cls.is_empty() {
            return Ok(Vec::new());
        }
        // size()/get(int) — avoids re-entering the iterator natives.
        // Short-circuited; see `iterate_object`'s twin of this pair. An arena receiver without a
        // `size()` is the registry/`Iterable` shape this whole function exists to serve, so the
        // discarded `get(int)` walk was the COMMON case here, not the rare one.
        let szm = self.resolve_method(cls, "size", "()I")?;
        let getm = if szm.is_some() {
            self.resolve_method(cls, "get", "(I)Ljava/lang/Object;")?
        } else {
            None
        };
        if let (Some(sz), Some(getm)) = (szm, getm) {
            let n = self
                .invoke(sz, "size", "()I", vec![recv.clone()])?
                .and_then(|v| v.as_int())
                .unwrap_or(0);
            if (0..5_000_000).contains(&n) {
                // Constant of the drain, hoisted: every one of the `n` `get(i)` calls below used
                // to re-hash `"get"`+descriptor and clone/drop the declaring class `Arc`.
                let gi = iter_midx(&getm, "get", "(I)Ljava/lang/Object;");
                // next-gen-gc: each `get` invoke can move the arena elements via a
                // collection. A plain Rust-local `out` Vec is NOT a GC root, so values
                // accumulated earlier keep their pre-move (now stale) addresses — the
                // 3c stackful recipe-load `TaggedChoice` stale-key. Accumulate in
                // `gc_protect` instead (forwarded in place on every collection), then
                // hand back the now-current values. (Default heap GC is non-moving, so
                // the plain path stays correct + byte-identical there.)
                #[cfg(feature = "next-gen-gc")]
                {
                    let base = crate::vm::thread_context::root_len(&self.gc_protect);
                    for i in 0..n {
                        let v = self
                            .invoke_idx(&getm, gi, "get", "(I)Ljava/lang/Object;", vec![recv.clone(), Value::Int(i)])?
                            .unwrap_or(Value::Null);
                        crate::vm::thread_context::push_root(&mut self.gc_protect, v);
                    }
                    return Ok(crate::vm::thread_context::split_off_roots(
                        &mut self.gc_protect,
                        base,
                    ));
                }
                #[cfg(not(feature = "next-gen-gc"))]
                {
                    let mut out = Vec::with_capacity(n as usize);
                    for i in 0..n {
                        out.push(
                            self.invoke_idx(&getm, gi, "get", "(I)Ljava/lang/Object;", vec![recv.clone(), Value::Int(i)])?
                                .unwrap_or(Value::Null),
                        );
                    }
                    return Ok(out);
                }
            }
        }
        // Else drain via REAL iterator() bytecode (resolve; None ⇒ no bytecode ⇒
        // give up, never recurse back through the iterator natives).
        let it = match self.resolve_method(cls, "iterator", "()Ljava/util/Iterator;")? {
            Some(decl) => self
                .invoke(decl, "iterator", "()Ljava/util/Iterator;", vec![recv.clone()])?
                .unwrap_or(Value::Null),
            None => return Ok(Vec::new()),
        };
        // A native iterator carries its elements in `$elems` from `$pos`.
        if let Value::Ref(ih) = it {
            if let Value::Array(arr) = self.heap.get_field_syn(ih, crate::heap::Syn::Elems) {
                let mut all = self.heap.elems_values(ih, arr);
                let pos = self.heap.get_field_syn(ih, crate::heap::Syn::Pos).as_int().unwrap_or(0).max(0) as usize;
                // Same in-place `drain` as the `iterate_object` twin of this arm: `skip().collect()`
                // built a SECOND `Vec` for the whole remainder on every drain, including the
                // `pos == 0` case where it copied every element to change nothing.
                if pos > 0 {
                    all.drain(..pos.min(all.len()));
                }
                return Ok(all);
            }
        }
        // Generic hasNext()/next() drain over the iterator value (Ref or arena Obj).
        // Borrowed, not `to_string()`d: a heap `Ref` iterator reads its name straight out of the
        // object header, an arena one out of the class registry's `Arc<str>`.
        #[cfg(feature = "next-gen-gc")]
        let icls_arc = match &it {
            Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)),
            _ => None,
        };
        #[cfg(not(feature = "next-gen-gc"))]
        let icls_arc: Option<Arc<str>> = None;
        let (hn, nx) = {
            let icls: &str = match &it {
                Value::Ref(h) => self.heap.class_of(*h).unwrap_or(""),
                _ => icls_arc.as_deref().unwrap_or(""),
            };
            // Short-circuited; see `drain_arena_iterator`.
            let Some(hn) = self.resolve_method(icls, "hasNext", "()Z")? else {
                return Ok(Vec::new());
            };
            let Some(nx) = self.resolve_method(icls, "next", "()Ljava/lang/Object;")? else {
                return Ok(Vec::new());
            };
            (hn, nx)
        };
        // Constants of THIS iterator, hoisted out of the per-element loops below — see
        // `iter_midx` for what each `hasNext`/`next` step used to re-derive.
        let (hni, nxi) = (
            iter_midx(&hn, "hasNext", "()Z"),
            iter_midx(&nx, "next", "()Ljava/lang/Object;"),
        );
        // next-gen-gc: root BOTH the iterator and the accumulated elements in
        // gc_protect across the hasNext/next invokes (any of which can move arena
        // objects) — slot `base` holds the (forwarded-in-place) iterator, `base+1..`
        // the drained elements. A plain `out`/`it` Rust local would keep stale
        // pre-move addresses. (Wall: 3c stackful TaggedChoice stale recipe-key.)
        #[cfg(feature = "next-gen-gc")]
        {
            let base = crate::vm::thread_context::root_len(&self.gc_protect);
            crate::vm::thread_context::push_root(&mut self.gc_protect, it);
            loop {
                let it_now =
                    crate::vm::thread_context::root_index(&self.gc_protect, base);
                let has = self
                    .invoke_idx(&hn, hni, "hasNext", "()Z", vec![it_now])?
                    .and_then(|v| v.as_int())
                    .unwrap_or(0);
                if has == 0 {
                    break;
                }
                let it_now =
                    crate::vm::thread_context::root_index(&self.gc_protect, base);
                let v = self
                    .invoke_idx(&nx, nxi, "next", "()Ljava/lang/Object;", vec![it_now])?
                    .unwrap_or(Value::Null);
                crate::vm::thread_context::push_root(&mut self.gc_protect, v);
                if crate::vm::thread_context::root_len(&self.gc_protect) - base > 2_000_001 {
                    break;
                }
            }
            let out: Vec<Value> = crate::vm::thread_context::split_off_roots(
                &mut self.gc_protect,
                base + 1,
            );
            crate::vm::thread_context::truncate_roots(&mut self.gc_protect, base); // drop the iterator slot
            return Ok(out);
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            let mut out = Vec::new();
            for _ in 0..2_000_000 {
                let has = self
                    .invoke_idx(&hn, hni, "hasNext", "()Z", vec![it.clone()])?
                    .and_then(|v| v.as_int())
                    .unwrap_or(0);
                if has == 0 {
                    break;
                }
                out.push(
                    self.invoke_idx(&nx, nxi, "next", "()Ljava/lang/Object;", vec![it.clone()])?
                        .unwrap_or(Value::Null),
                );
            }
            Ok(out)
        }
    }
}

// ═══════ CMP-CALLER: the entry marks, the direct lambda call, and the direct sort ═══════
impl Vm {
    pub(crate) const CMP_EP_LIST_SORT: u8 = 1;
    pub(crate) const CMP_EP_COLLECTIONS_SORT: u8 = 2;
    pub(crate) const CMP_EP_ARRAYS_SORT: u8 = 3;
    pub(crate) const CMP_EP_STREAM_SORTED: u8 = 4;
    pub(crate) const CMP_EP_SORTED_SET_ORDER: u8 = 5;
    pub(crate) const CMP_EP_BRIDGE: u8 = 6;
    pub(crate) const CMP_EP_STREAM_MINMAX: u8 = 7;
    pub(crate) const CMP_EP_COLLECTIONS_MINMAX: u8 = 8;
    pub(crate) const CMP_EP_MINMAX_BY: u8 = 9;
    pub(crate) const CMP_EP_BINARY_SEARCH: u8 = 10;
    pub(crate) const CMP_EP_NAVIGABLE: u8 = 11;

    /// Mark the compares made while the returned guard lives as belonging to entry `ep`, over a
    /// collection of `n` elements. Nesting is honoured (a comparator body that sorts restores the
    /// outer mark on drop). Inert under the kill switch.
    #[inline]
    pub(crate) fn cmp_entry(ep: u8, n: usize) -> CmpEntryScope {
        if !cc_on() || ep as usize >= EP_COUNT {
            return CmpEntryScope { prev: 0, start: 0, n: 0, ep: 0, armed: false };
        }
        let prev = CC_EP.with(|c| c.replace(ep));
        let start = CC_N.with(|c| c.get());
        CmpEntryScope { prev, start, n: n.min(u32::MAX as usize) as u32, ep, armed: true }
    }

    /// Resolve the lambda behind handle `h` for direct calls, once per site: the index of its
    /// `DirectLam` on this thread, or `NO_DIRECT` when the shape is refused (see the block
    /// comment's GUARD). Reads only; runs no guest code.
    fn direct_lam_for(&self, h: u32) -> u32 {
        if !cc_on()
            || !Self::fast_indyfam_enabled()
            || crate::env_flag!("RAVA_LAMBDA_TRACE")
            || crate::env_flag!("RAVA_LAMBDA_STAT")
        {
            return NO_DIRECT;
        }
        // `RAVA_GC_STATS` prices the handle lambda's `lambda_of` snapshot on the historic path;
        // a call that never takes that snapshot would read as a saving in its ledger.
        #[cfg(feature = "next-gen-gc")]
        if crate::nextgen::gc_stats_enabled() {
            return NO_DIRECT;
        }
        // Kind 6 = REF_invokeStatic: the dispatch owner IS the site's own interned owner, which is
        // the condition under which `invoke_lambda_parts` consults its site plan at all.
        let site = match self.heap.lambda_header(h) {
            Some((6, site, _)) => Arc::clone(site),
            _ => return NO_DIRECT,
        };
        if &*site.owner == "$Lambda"
            || crate::opcodes::force_native_collection(self, &site.owner, &site.name, &site.descriptor)
        {
            return NO_DIRECT;
        }
        let decl = match self.resolve_method(&site.owner, &site.name, &site.descriptor) {
            Ok(Some(decl)) => decl,
            _ => return NO_DIRECT,
        };
        let Some(midx) = crate::tlcache::method_index(&decl, &site.name, &site.descriptor) else {
            return NO_DIRECT;
        };
        if decl.file.methods.get(midx as usize).and_then(|m| m.code.as_ref()).is_none() {
            return NO_DIRECT;
        }
        DIRECT.with(|t| {
            let mut t = t.borrow_mut();
            if let Some(i) = t.iter().position(|d| Arc::ptr_eq(&d.site, &site)) {
                return i as u32;
            }
            if t.len() >= DIRECT_CAP {
                return NO_DIRECT;
            }
            t.push(std::rc::Rc::new(DirectLam {
                site,
                decl,
                midx,
                compile_requested: std::cell::Cell::new(false),
            }));
            (t.len() - 1) as u32
        })
    }

    /// Call the lambda behind `h` — whose site `dl` resolved — with its captures followed by
    /// `a` (and `b`), through `invoke_idx`. `Refused` when the header is not the one `dl` was
    /// derived from or a diagnostic is armed: nothing has run, the caller takes the historic arm.
    fn call_direct(
        &mut self,
        dl: &std::rc::Rc<DirectLam>,
        h: u32,
        a: Value,
        b: Option<Value>,
    ) -> Result<Direct, VmError> {
        if self.trace.lambda {
            return Ok(Direct::Refused);
        }
        // The pooled buffer `invoke_idx` expects to be handed (and hands back): no malloc for
        // the argument vector, where the historic arm performed two or three.
        let mut args = self.take_vec();
        let ok = match self.heap.lambda_header(h) {
            Some((6, site, captured)) if Arc::ptr_eq(site, &dl.site) => {
                args.reserve(captured.len() + 2);
                // Captures first, then the SAM arguments — `splice_call_args`' order.
                args.extend_from_slice(captured);
                true
            }
            _ => false,
        };
        if !ok {
            self.give_vec(args);
            return Ok(Direct::Refused);
        }
        args.push(a);
        if let Some(b) = b {
            args.push(b);
        }
        // Rooted for the whole call, as `invoke_lambda_parts` roots its `call_args`.
        #[cfg(feature = "next-gen-gc")]
        let _gc_root = crate::opcodes::push_call_roots(self, None, &args);
        // `flatcall` bypasses `jit_run` for its body, so apply the fixed first-use policy here.
        #[cfg(feature = "next-gen-gc")]
        if !dl.compile_requested.get()
            && crate::flatcall::lambda_compilation_enabled()
            && !crate::gil::is_worker()
        {
            dl.compile_requested.set(true);
            if let Some(code) =
                dl.decl.file.methods.get(dl.midx as usize).and_then(|m| m.code.as_ref())
            {
                self.flat_compile_first_use(&dl.decl, &dl.site.name, &dl.site.descriptor, code);
            }
        }
        let r = self.invoke_idx(&dl.decl, Some(dl.midx), &dl.site.name, &dl.site.descriptor, args)?;
        Ok(Direct::Ran(r))
    }

    /// One key-function application of the K_KEY arm: direct when `dx` names a resolved site
    /// (`kfh` is then the key function's handle), the historic `apply` otherwise. Same body, same
    /// single argument, same `unwrap_or(Null)` of a void return.
    #[inline]
    fn apply_key(&mut self, dx: u32, kfh: u32, kf: &Value, arg: &Value) -> Result<Value, VmError> {
        if let Some(dl) = direct_get(dx) {
            if let Direct::Ran(v) = self.call_direct(&dl, kfh, arg.clone(), None)? {
                cc_bump(CC_SERVED + SV_KEY);
                return Ok(v.unwrap_or(Value::Null));
            }
        }
        Ok(self.apply(kf, vec![arg.clone()])?.unwrap_or(Value::Null))
    }

    /// `sort_values` for a heap-handle LAMBDA comparator whose site resolves (see the block
    /// comment): the historic bottom-up stable merge — the same pairs compared in the same order,
    /// `<= 0` taking the left run — with the comparator resolved once and called directly. `None`
    /// is a refusal: nothing has run, nothing has changed, and the historic `sort_values` body
    /// runs from the top. Elements live in a rooted heap `Object[]` across the comparator's guest
    /// code and are read back through it, as `ComparatorPlan::sort_memoised` does.
    pub(crate) fn sort_values_direct(
        &mut self,
        elems: &mut [Value],
        cmp: Option<&Value>,
    ) -> Option<Result<(), VmError>> {
        if !cc_on() || elems.len() < 2 {
            return None;
        }
        let h = match cmp {
            Some(Value::Ref(h)) => *h,
            _ => return None,
        };
        if !self.heap.is_lambda(h) {
            return None;
        }
        let dl = direct_get(self.direct_lam_for(h))?;
        Some(self.sort_direct(elems, h, &dl))
    }

    fn sort_direct(
        &mut self,
        elems: &mut [Value],
        h: u32,
        dl: &std::rc::Rc<DirectLam>,
    ) -> Result<(), VmError> {
        let n = elems.len();
        let src = self.heap.alloc_values(crate::stream::OBJ, elems.to_vec());
        let _root = crate::stream::gc_protect_array(self, src);
        let order = crate::comparator_plan::merge_sort_indices(n, |i, j| {
            let va = self.heap.array_get(src, i).unwrap_or(Value::Null);
            let vb = self.heap.array_get(src, j).unwrap_or(Value::Null);
            cc_note(CK_LAMBDA, &va, &vb);
            match self.call_direct(dl, h, va, Some(vb))? {
                Direct::Ran(v) => {
                    cc_bump(CC_SERVED + SV_SORT);
                    Ok(v.and_then(|v| v.as_int()).unwrap_or(0))
                }
                // Cannot happen mid-sort (the header is immutable and no trace flag flips), but
                // the refusal is honoured all the same: the historic compare for this pair.
                Direct::Refused => {
                    let va = self.heap.array_get(src, i).unwrap_or(Value::Null);
                    let vb = self.heap.array_get(src, j).unwrap_or(Value::Null);
                    self.compare(&Value::Ref(h), &va, &vb)
                }
            }
        })?;
        for (slot, &i) in elems.iter_mut().zip(order.iter()) {
            *slot = self.heap.array_get(src, i).unwrap_or(Value::Null);
        }
        Ok(())
    }
}
