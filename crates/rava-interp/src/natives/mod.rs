//! JDK native layer. The jar bundles application/library `.class` files but not
//! the `java.*` runtime, so a core set of JDK methods must be provided
//! intrinsically (exactly the role a JVM's native/bootstrap layer plays). This
//! grows method-by-method against real frontiers — never speculatively.
//!
//! Dispatch contract (`try_invoke`):
//!   Ok(None)        -> not a known native; caller raises UnsupportedNative
//!   Ok(Some(ret))   -> handled; `ret` is the (optional) return value
//!
//! `args[0]` is the receiver for instance methods.


















pub mod bridgestat;
pub(crate) mod served_census;
mod gatedump;
pub use gatedump::*;
use crate::value::Value;
use crate::{Vm, VmError};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// Compiled-regex cache keyed by pattern. Java's `String.split/matches/replaceAll` and
/// `Pattern`/`Matcher` recompile a regex per call; MC parses resource locations constantly, so
/// rebuilding the engine + its aho-corasick literal prefilter dominated boot CPU. `None` caches
/// "won't compile".
pub(crate) type ReMap = HashMap<Box<str>, Option<Arc<regex::Regex>>>;

thread_local! {
    /// ⭐**A `thread_local` LOWERS TO A BARE TLS-SLOT LOAD ONLY WHEN IT IS BOTH CONST-INITIALISED
    /// AND DESTRUCTOR-FREE** — the shape two of this session's winning rounds were built on.
    ///
    /// This slot held the `HashMap` itself, so it was NEITHER: `HashMap::new()` is not a const
    /// initialiser (its `RandomState` is not), and the map owns `Box<str>` keys and `Arc<Regex>`
    /// values, so it carries drop glue. Every `.with()` therefore ran the lazy-init state check
    /// AND the "has this thread registered its destructor yet" path — twice per compile, once per
    /// cache hit, on every regex-backed native (`String.matches`/`replaceAll`/`split` with a
    /// non-literal pattern, and every `Matcher` operation, which `misc::matcher_dispatch` reaches
    /// TWICE for `matches`).
    ///
    /// A `Cell<Option<&'static ...>>` is const-initialisable and `Copy`, i.e. destructor-free, so
    /// the slot is now a plain pointer read. The map moves behind a leaked `&'static RefCell`,
    /// which keeps the borrow checking exactly as it was — this is a REPRESENTATION change with
    /// no observable behaviour, so it carries no kill switch of its own: there is nothing to
    /// switch. Retention is unchanged for a live thread (nothing was ever evicted from this map);
    /// what a thread no longer does is free its compiled regexes at exit, and the threads that
    /// reach here are the long-lived pool.
    pub(crate) static RE_CACHE: std::cell::Cell<Option<&'static RefCell<ReMap>>> =
        const { std::cell::Cell::new(None) };
}

/// This thread's regex cache, through one TLS-slot load on the hit path.
#[inline]
fn re_cache() -> &'static RefCell<ReMap> {
    match RE_CACHE.with(|c| c.get()) {
        Some(m) => m,
        None => {
            let m: &'static RefCell<ReMap> = Box::leak(Box::new(RefCell::new(ReMap::default())));
            RE_CACHE.with(|c| c.set(Some(m)));
            m
        }
    }
}

/// Compile a regex through the thread-local cache. `None` = does not compile
/// (callers fall back to literal handling).
/// Rewrite Java's \\Q...\\E blocks into per-character escapes the `regex` crate accepts.
/// Everything outside a block passes through untouched; an unterminated block runs to the
/// end of the pattern, as Java's does.
fn translate_quote_blocks(pat: &str) -> String {
    if !pat.contains("\\Q") {
        return pat.to_string();
    }
    let mut out = String::with_capacity(pat.len());
    let mut rest = pat;
    while let Some(i) = rest.find("\\Q") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let (lit, tail) = match after.find("\\E") {
            Some(j) => (&after[..j], &after[j + 2..]),
            None => (after, ""),
        };
        out.push_str(&regex::escape(lit));
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// ⛔**THE ARC CLONE HERE IS A HAND-OFF, NOT AN OVERSIGHT.** The `.cloned()` below is exactly the
/// shape this round hunts — an `Arc` cloned out of a table (two atomic read-modify-writes) only to
/// be read through and dropped by the caller — and the cure is to store `&'static regex::Regex`
/// (the map is leaked and never evicted, so the reference is genuinely `'static`) and return
/// `Option<&'static regex::Regex>`. Every one of the six call sites uses the result only through
/// autoderef (`re.is_match`, `re.captures_at`, `re.replace_all`, and one `split_pattern(&re, ..)`
/// where `&&Regex` deref-coerces), so no call site needs editing — but all six live in
/// `natives/misc.rs`, `natives/strings.rs` and `natives/text.rs`, which belong to other agents in
/// this same tree, and a signature they consume is not mine to change blind. Population: two
/// atomics per regex-backed native call, and `matcher_dispatch` makes two such calls for
/// `Matcher.matches`.
pub(crate) fn cached_regex(pat: &str) -> Option<Arc<regex::Regex>> {
    let cache = re_cache();
    if let Some(v) = cache.borrow().get(pat).cloned() {
        return v;
    }
    // JAVA'S \\Q...\\E LITERAL BLOCKS, which the `regex` crate does not know. They come
    // from `Pattern.quote`, and a guest that quotes a string and then matches on it got
    // NO MATCH — the two halves of one idiom disagreeing. Translate to escaped literals
    // before compiling.
    let translated = translate_quote_blocks(pat);
    let compiled = regex::Regex::new(&translated).ok().map(Arc::new);
    cache.borrow_mut().insert(pat.into(), compiled.clone());
    compiled
}

/// Unbox a boolean: a boxed `java/lang/Boolean` object carries its 0/1 in
/// `$value`; an unboxed primitive is already a `Value::Int`. Lets natives accept
/// either form for a boolean argument/receiver.
pub(crate) fn bool_of(vm: &Vm, v: Option<&Value>) -> i32 {
    match v {
        Some(Value::Ref(h)) => vm.heap.get_field(*h, "$value").as_int().unwrap_or(0),
        Some(x) => x.as_int().unwrap_or(0),
        None => 0,
    }
}

/// The payload of a boxed `Character`/`Short`/`Byte`, or `None` for anything that is not one.
///
/// ⚠**THE CLASS TEST IS NOT OPTIONAL.** `bool_of` above reads `$value` off ANY handle because a
/// `Z` argument can only ever be a boolean; this helper is reached from consumers that take
/// arbitrary objects (`equals`, `hashCode`, a map key), and a `$value`-less object would read
/// `Value::Null` → 0 and answer as if it were the character NUL. Keyed on the class so a miss is
/// a decline, never a zero. `Boolean` is deliberately excluded — it has `bool_of`.
#[inline]
pub(crate) fn boxed_narrow(vm: &Vm, v: Option<&Value>) -> Option<(crate::boxid::Kind, i32)> {
    let Some(Value::Ref(h)) = v else { return None };
    let kind = crate::boxid::Kind::of_class(vm.heap.class_of(*h)?)?;
    let n = vm.heap.get_field(*h, "$value").as_int()?;
    crate::boxid::note_unbox(kind);
    Some((kind, n))
}

/// ⭐**THE ONE UNBOXER EVERY NARROW-BOX CONSUMER TAKES.** `Value::as_int` answers `None` for a
/// `Value::Ref`, so every arm that used to read a boxed char/short/byte with
/// `as_int().unwrap_or(0)` would silently read ZERO once the box became a real object. This reads
/// the payload out of a box and falls through to `as_int` for the unboxed primitive — so a site
/// converted to it behaves identically for every input it could receive BEFORE this work, and
/// correctly for the one it can receive after.
#[inline]
pub(crate) fn int_of(vm: &Vm, v: Option<&Value>) -> Option<i32> {
    match boxed_narrow(vm, v) {
        Some((_, n)) => Some(n),
        None => v.and_then(Value::as_int),
    }
}

/// [`int_of`] with the historic `unwrap_or(0)` folded in, for the arms that already defaulted
/// a missing argument to zero.
#[inline]
pub(crate) fn int_of_or0(vm: &Vm, v: Option<&Value>) -> i32 {
    int_of(vm, v).unwrap_or(0)
}

/// `char_of` / `short_of` / `byte_of` — [`int_of`] named for the kind the call site expects, so
/// an arm reads as the JDK method it implements. All three share one body: the box's `$value` is
/// already narrowed at mint time ([`crate::Vm::narrow_box`]).
#[inline]
pub(crate) fn char_of(vm: &Vm, v: Option<&Value>) -> i32 {
    int_of_or0(vm, v)
}
/// See [`char_of`].
#[inline]
pub(crate) fn short_of(vm: &Vm, v: Option<&Value>) -> i32 {
    int_of_or0(vm, v)
}
/// See [`char_of`].
#[inline]
pub(crate) fn byte_of(vm: &Vm, v: Option<&Value>) -> i32 {
    int_of_or0(vm, v)
}

/// The widened payload of any box or primitive, for the `Number` cross-conversion arm: a narrow
/// box unboxes, everything else keeps `as_long`'s answer.
#[inline]
pub(crate) fn long_of(vm: &Vm, v: Option<&Value>) -> i64 {
    match boxed_narrow(vm, v) {
        Some((_, n)) => n as i64,
        None => v.and_then(Value::as_long).unwrap_or(0),
    }
}

/// Map$Entry getKey/getValue early fast-path (DEFAULT ON, opt-out RAVA_NO_ENTRY_FASTPATH).
fn entry_fastpath_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ENTRY_FASTPATH").is_none())
}

/// `Optional.of/ofNullable/empty` early fast-path — a trivial-work native that
/// otherwise pays the full owner scan (DEFAULT ON, opt-out RAVA_NO_TRIVIAL_FASTPATH).
fn trivial_fastpath_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_TRIVIAL_FASTPATH").is_none())
}

/// The `owner_bits` family memo is live: BOTH `owner_bits::enabled()` and
/// `owner_bits::id_resolve()` said yes. See [`nat_gates`].
pub(crate) const NG_FAM_BY_ID: u8 = 1 << 0;
/// KILL SWITCH `RAVA_NO_NAT_GATEWORD=1` (or `RAVA_NO_EMIT_MORE`): every folded constant is read
/// back at its own call site, from its own static, exactly as it was before this round. The word
/// is still built — it has to be, to carry this bit — so the switch restores the READS, which is
/// what is being measured, not the answers, which never differed.
pub(crate) const NG_LEGACY: u8 = 1 << 1;

/// ⭐**ONE READ FOR THE NATIVE SURFACE'S OWN PROCESS CONSTANTS** — the `entry_gates()` /
/// `cascade_gates()` shape, applied to the last layer of the cascade that still read its gates
/// one static at a time.
///
/// WHAT IT REPLACES, AND ON WHAT POPULATION. `cascade_owners::owner_families` is the second half
/// of the native cascade and it runs on EVERY cascade entry that gets past the head ladder —
/// order 6 M per product boot (`RAVA_DISPATCH_GATE`'s own census read 7.64 M cascade entries with
/// the gate off, 6.08 M with it on). Its first statement was
///
/// ```text
///     let by_id = owner_bits::enabled() && owner_bits::id_resolve();
/// ```
///
/// two separate `OnceLock<bool>`s, i.e. two ACQUIRE-ordered atomic loads from two addresses and
/// two branches, per entry, to learn that the default configuration is the default configuration.
/// An acquire load is not a free `mov`: LLVM may not hoist it out of the caller's loop and may not
/// forward one across the opaque dispatcher calls that sit between these sites, so two statics
/// really are two loads. Folded here they are ONE load of one word and two predicted bit tests.
///
/// **PER-ENTRY ACQUIRE LOADS: 2 → 1** on the `owner_families` population.
///
/// The family word read adds no further gate loads: `owner_families` calls the gate-free
/// `owner_bits::family_word(owner)` — the `plan()`/`bits()` accessor's sibling for the
/// entry's second word, with no gate reads at all — only inside its `by_id` arm, so the
/// caller has already proved `enabled() && id_resolve()` before the memo is touched.
///
/// Every bit is the SAME predicate read from the SAME accessor, so an armed or kill-switched run
/// behaves exactly as it did.
#[inline]
pub(crate) fn nat_gates() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u8;
        if crate::owner_bits::enabled() && crate::owner_bits::id_resolve() {
            w |= NG_FAM_BY_ID;
        }
        // Falls together with the other emit-more switches, as every gate in this tree does.
        if std::env::var_os("RAVA_NO_NAT_GATEWORD").is_some() || crate::flags::emit_more_disabled()
        {
            w |= NG_LEGACY;
        }
        w
    })
}

/// Native Iterator hasNext/next early fast-path (DEFAULT ON, opt-out RAVA_NO_ITER_FASTPATH).
fn iter_fastpath_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ITER_FASTPATH").is_none())
}

/// Owner gate on the dispatch gauntlet. DEFAULT-OFF pending its A/B (`RAVA_DISPATCH_GATE`
/// enables). Its author promoted it to default-ON on a structural argument - it only removes
/// work (81.8M sub-dispatcher entries on ChunkFill) and adds no per-call lookup - while
/// stating outright that no wall A/B was run. Same rule as the lambda splice: measured
/// separation ships a default, structure does not.
/// ⛔**MEASURED AND LEFT OPT-IN (2026-08-29).** The self-time meters put the stage gauntlet at
/// 5.55% of a boot and this gate is the mechanism built to skip it, so it looked like the one
/// lever the decomposition had found. It is correct — `RAVA_DISPATCH_GATE_VERIFY` over a FULL
/// product boot reported **zero** arms that would have served a rejected owner, and 4 bit-exact
/// gates and 21 differential probes pass with it on — and it fires hard on the counts:
///
/// ```text
///                    gate OFF            gate ON
/// cascade entries    7.64 M              6.08 M     (-20%)
/// service stages     5.18 M / 2.72%      3.77 M / 1.07%
/// map stage          3.23 M              0.76 M     (-76%)
/// natives, total     14.2%               13.7%
/// ```
///
/// And the boot does not move: **+0.25% CPU** over six alternated product boots. Declining a
/// stage was already cheap, so removing the decline buys nothing — the same answer the seam, the
/// arm cache and the birth lines all gave this session.
///
/// It stays opt-in rather than becoming the default, because the gate is a SUPERSET claim over
/// six modules' arm conditions and a future edit to any of them can silently break it. Carrying
/// that fragility is worth it for a win; it is not worth it for 0.25%.
fn dispatch_gate_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DISPATCH_GATE").is_some())
}

/// RAVA_DISPATCH_GATE_VERIFY (default OFF, inert): run the gauntlet even for an
/// owner the gate rejected, report any arm that WOULD have served it, and tally
/// how much of the gauntlet the gate removes. The gate is a superset claim over
/// six modules' arm conditions; this is how a future change to any of them gets
/// caught on a real workload instead of in a boot log.
pub fn dispatch_gate_verify() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DISPATCH_GATE_VERIFY").is_some())
}

/// Sub-dispatcher stages the plan can gate (nio, net, stream, reflect, map, time).
const STAGE_COUNT: u64 = 6;

/// Gate tally (verify mode only): owner-cascade entries, and how many of them the
/// owner alone decides. Counts, not walls.
static GATE_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GATE_SKIPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GATE_SERVED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Blocked by the owner-agnostic NAME list rather than by the owner itself.
static GATE_NAME_BLOCKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Sub-dispatcher entries the plan removes, summed over all cascade entries.
static GATE_STAGES_SKIPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Per-(owner,name) census of what actually reaches the cascade — the only way to
/// aim a dispatch fix, since a share of `dispatch_owner` says nothing about WHICH
/// owners pay it. Keyed by the &str pointers (constant-pool / super-chain backing,
/// stable for process lifetime); the label is copied once on first insert.
type GateCensus = HashMap<(usize, usize), (String, u64)>;
/// Method names some dispatcher serves with NO condition on the owner, so the
/// owner gate must never skip them:
///   * `ordinal`/`name`/`compareTo` — the synthetic-enum accessors below read
///     `$enumName`/`$enumOrd` off any receiver, whatever its class.
///   * `spliterator`/`stream`/`iterator`/`forEach` — `stream::dispatch` drains any
///     Iterable receiver through these before it looks at the owner at all.
///   * `inEventLoop`/`execute` — the netty event-executor verdicts are structural
///     (net/mod.rs's provider tables: the `EventExecutorGroup` closure + the exact
///     SslHandler and ThreadPerTaskExecutor tuples), so their owners live in no
///     fixed namespace and the cascade's net stage serves the names whatever the
///     owner.
/// Every other arm in the gauntlet is keyed on the owner; see `owner_bits::may_serve`.
#[inline]
fn owner_agnostic_name(name: &str) -> bool {
    matches!(
        name,
        "ordinal" | "name" | "compareTo" | "spliterator" | "stream" | "iterator" | "forEach"
            | "inEventLoop"
            | "execute"
    )
}

/// The Iterable defaults `stream::dispatch` serves on ANY receiver, before it
/// looks at the owner (stream.rs, the `needs_iter` fast paths).
#[inline]
fn stream_iterable_name(name: &str) -> bool {
    matches!(name, "spliterator" | "stream" | "iterator" | "forEach")
}

/// LAST-ARM SHORTCUT for the native cascade.
///
/// `dispatch_owner_cascade` is a LINEAR SCAN: six stage dispatchers followed by ~42 inline
/// `owner ==` comparisons, and a class's cost is decided by where it sits in that list.
/// Measured (ledger row 174, PC profile of ONE call): `StringBuilder.length()` costs 718 ns
/// against `ArrayList.size()`'s 66 ns for identical trivial work, and `sb_dispatch` — the
/// function that actually reads the length — is **1.6% of the call**. The other 98.4% is
/// finding it.
///
/// Two name-keyed fixes were tried and both failed, which is why this one is keyed on nothing:
/// a content-verified last-owner memo in `owner_bits::bits` measured NEUTRAL (the super-chain
/// walk changes the owner between consecutive calls), and `RAVA_DISPATCH_GATE`'s stage bitmask
/// measured a REGRESSION (computing the plan costs the same name lookup it saves).
///
/// ★ SOUNDNESS, and it is the whole reason this is safe to land: the shortcut RE-RUNS the arm's
/// own owner condition before calling its dispatcher, and falls through to the full cascade on
/// any miss. A stale or wrong arm id can therefore only cost a redundant compare — it can never
/// serve the wrong owner. The cache needs no key, no invalidation and no guard beyond that.
///
/// Only arms whose body is a single dispatcher call are listed; arms with inline bodies stay in
/// the scan until they are extracted the same way.
const ARM_NONE: u8 = 0;
const ARM_INT: u8 = 1;
const ARM_STR: u8 = 2;
const ARM_WRITER: u8 = 3;
const ARM_SB: u8 = 4;

/// Direct-mapped, keyed by the (owner, name) POINTER PAIR. Not a hash map: row 178's rule is
/// that the cost in this layer is LOCKS AND HASHES, so this is a shift/xor into a fixed array.
const ARM_SLOTS: usize = 256;
thread_local! {
    pub(crate) static ARM_CACHE: std::cell::RefCell<[(u64, u8); ARM_SLOTS]> =
        const { std::cell::RefCell::new([(0, ARM_NONE); ARM_SLOTS]) };
}

/// Pointer-pair key. Both strings come from constant pools / interned names that outlive the
/// call, and a stale key can only produce a MISS (the arm re-checks and the cascade re-runs),
/// so no verification is needed beyond what the arm already does.
#[inline]
fn arm_key(owner: &str, name: &str) -> u64 {
    (owner.as_ptr() as u64) ^ (name.as_ptr() as u64).rotate_left(32)
}

#[inline]
fn note_arm_for(owner: &str, name: &str, a: u8) {
    let k = arm_key(owner, name);
    let slot = ((k >> 5) as usize) & (ARM_SLOTS - 1);
    ARM_CACHE.with(|c| c.borrow_mut()[slot] = (k, a));
}

/// `RAVA_ARM_STAT`: hits/misses of the shortcut, so "it did not help" can be told apart from
/// "it did not fire" — the distinction four dead instruments in this ledger were read without.
pub(crate) static ARM_HIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static ARM_MISS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Resolve a system property (subset). Unknown keys return None so callers can
/// apply their own default.
/// User-supplied system properties (`-Dkey=value` on the command line). Set once
/// by the CLI before the VM runs; read by `property()` ahead of the built-ins.
static USER_PROPS: std::sync::OnceLock<std::collections::HashMap<String, String>> =
    std::sync::OnceLock::new();

/// Install the `-D` properties parsed from the command line.
pub fn set_user_properties(props: std::collections::HashMap<String, String>) {
    let _ = USER_PROPS.set(props);
}

/// Allocate a `java/lang/Class` instance carrying the represented class name.
fn make_class_object(vm: &mut Vm, name: &str) -> Value {
    // Interned so getClass()/class literals yield one identity per class.
    vm.make_class(name)
}

/// Extract `(buf_handle, pos)` from an InputStream object.
/// Resolve a stream to its backing buffer: `(holder, buf, pos)`. Follows the `$stream`
/// wrapper chain (PushbackInputStream → BufferedInputStream → FileInputStream) to the
/// layer that actually owns `$buf`, so `$pos` is read/advanced on THAT layer — wrapper
/// layers (which only forward) hold no buffer of their own.
pub(crate) fn stream_state(vm: &Vm, recv: Option<&Value>) -> Option<(u32, u32, usize)> {
    let h = underlying_stream(vm, recv)?;
    let buf = match vm.heap.get_field(h, "$buf") {
        Value::Array(b) => b,
        _ => return None,
    };
    let pos = vm.heap.get_field_syn(h, crate::heap::Syn::Pos).as_int().unwrap_or(0) as usize;
    Some((h, buf, pos))
}

/// DataInputStream reads: pull `n` bytes (big-endian) from the underlying stream.
fn data_read_bytes(vm: &mut Vm, recv: Option<&Value>, n: usize) -> Result<Vec<u8>, VmError> {
    let Some(stream) = underlying_stream(vm, recv) else { return Ok(Vec::new()) };
    let sv = Some(Value::Ref(stream));
    // DEFAULT ON (`RAVA_NO_BULK_READ` opts out), and the flag is what settled it.
    //
    // The per-byte loop below costs SIX heap operations to move one byte: `$pushback`
    // probed by NAME, the underlying stream re-resolved, `$buf` and `$pos` read, a `Value`
    // round trip through `array_get`, and `$pos` written back. Reading the slice once cuts
    // the save gate's parse phase from 2,178 ms to 697 ms (3.1×).
    //
    // ⚠TWO SEPARATE-BINARY A/Bs READ THIS AS +17.85% OF BOOT `Done` AND BOTH WERE WRONG
    // (ledger rows 266-269). A same-binary flag A/B — identical build, identical layout,
    // alternating, n=4 — reads wall −0.44%, cpu −0.66%, done −0.05%. When the two designs
    // disagree, the flag A/B wins: it is the only one where the arms differ in nothing but
    // the executed path.
    static BULK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *BULK.get_or_init(|| std::env::var_os("RAVA_NO_BULK_READ").is_none()) {
        let pushed = match vm.heap.get_field(stream, "$pushback") {
            Value::Array(a) => vm.heap.array_len(a).unwrap_or(0) > 0,
            _ => false,
        };
        if !pushed {
            if let Some((sh, buf, pos)) = stream_state(vm, recv) {
                if let Some(bytes) = vm.heap.array_read_bytes(buf, pos, n) {
                    vm.heap.put_field_syn(sh, crate::heap::Syn::Pos, Value::Int((pos + bytes.len()) as i32));
                    return Ok(bytes);
                }
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let b = stream_read_one(vm, sv.as_ref())?;
        if b < 0 { break; }
        out.push(b as u8);
    }
    Ok(out)
}

fn be_i64(b: &[u8]) -> i64 {
    b.iter().fold(0i64, |acc, &x| (acc << 8) | x as i64)
}

/// Canonical decimal text for a BigDecimal/BigInteger constructor or factory arg.
fn bignum_text(vm: &Vm, v: Option<&Value>) -> String {
    match v {
        Some(Value::Int(i)) => i.to_string(),
        Some(Value::Long(l)) => l.to_string(),
        Some(Value::Double(d)) => fmt_plain_double(*d),
        Some(Value::Float(f)) => fmt_plain_double(*f as f64),
        _ => str_of(vm, v).unwrap_or_else(|| "0".to_string()),
    }
}

/// A double as a plain (non-exponential, for normal magnitudes) decimal string.
fn fmt_plain_double(d: f64) -> String {
    if !d.is_finite() { return "0".to_string(); }
    if d == d.trunc() && d.abs() < 1e15 { format!("{}", d as i64) } else { format!("{d}") }
}

/// `BigDecimal.scale()` from a decimal string: fractional-digit count minus the
/// `E`/`e` exponent (`"1.50"`→2, `"100"`→0, `"1E3"`→-3, `"1.5E2"`→-1, `"1E-3"`→3).
fn decimal_scale(s: &str) -> i32 {
    let s = s.trim();
    let (mantissa, exp) = match s.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (s, 0),
    };
    let frac = mantissa.split_once('.').map(|(_, f)| f.len() as i32).unwrap_or(0);
    frac - exp
}

/// `BigDecimal.precision()` — count of significant digits (leading zeros stripped;
/// an all-zero value has precision 1). `"123.45"`→5, `"0.012"`→2, `"100"`→3, `"0.0"`→1.
fn decimal_precision(s: &str) -> i32 {
    let mantissa = s.trim().trim_start_matches(['+', '-']).split(['e', 'E']).next().unwrap_or("");
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let stripped = digits.trim_start_matches('0');
    if stripped.is_empty() { 1 } else { stripped.len() as i32 }
}

/// RAVA_VALUEOF_PROBE support: per-argument-class tally of String#valueOf cost.
fn valueof_probe_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_VALUEOF_PROBE").is_some())
}
/// Follow a writer's `$out` chain to the object that actually holds `$sb`.
fn writer_target(vm: &Vm, h: u32) -> u32 {
    match vm.heap.get_field(h, "$out") {
        Value::Ref(o) => writer_target(vm, o),
        _ => h,
    }
}

fn writer_append(vm: &mut Vm, target: u32, text: &str) {
    vm.heap.sb_append(target, text); // amortized O(1) in-place append
}

/// String.regionMatches: does `s[toff..toff+len]` equal `other[ooff..ooff+len]`?
fn region_matches(s: &str, toff: i32, other: &str, ooff: i32, len: i32, _x: bool) -> bool {
    region_matches_ci(s, toff, other, ooff, len, false)
}

/// Flatten an `Object[]` varargs argument into a value vector.
pub(crate) fn varargs(vm: &Vm, arr: Option<&Value>) -> Vec<Value> {
    match arr {
        Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(),
        Some(v) => vec![v.clone()],
        None => Vec::new(),
    }
}

/// Insert `,` every three digits from the right (the `,` conversion flag under
/// `Locale.ROOT`, which is the only locale `format_java` models). Input must be
/// bare digits — the caller splits off any sign and fraction.
fn group_digits(digits: &str) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (k, c) in digits.chars().enumerate() {
        if k > 0 && (n - k) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn str_field_of(vm: &Vm, v: Option<&Value>, f: &str) -> String {
    match v { Some(Value::Ref(h)) => match vm.heap.get_field(*h, f) { Value::Str(s) => s.to_string(), _ => String::new() }, _ => String::new() }
}

pub(crate) fn field_or_null(vm: &Vm, v: Option<&Value>, f: &str) -> Value {
    match v { Some(Value::Ref(h)) => vm.heap.get_field(*h, f), _ => Value::Null }
}

/// java.math.RoundingMode enum constants, in declaration (ordinal) order.
const ROUNDING_MODES: [&str; 8] = ["UP", "DOWN", "CEILING", "FLOOR", "HALF_UP", "HALF_DOWN", "HALF_EVEN", "UNNECESSARY"];

/// java.util.Random / SecureRandom / ThreadLocalRandom — Java's 48-bit LCG,
/// seed stored on `$seed`. We have no entropy source, so a fresh Random is
/// seeded from a monotonic counter (deterministic across runs — fine for the
/// engine; callers that set a seed get exact Java results).
/// `rnd.nextInt(bound)` for a heap Random `r`, advancing its seed — so callers
/// (e.g. Collections.shuffle) reproduce the JDK's exact stream off the same
/// Random object. Routes through `random_dispatch` to share the LCG + seeding.
pub(crate) fn random_next_int_bound(vm: &mut Vm, r: u32, bound: i32) -> i32 {
    random_dispatch(vm, "nextInt", &[Value::Ref(r), Value::Int(bound)]).ok().flatten()
        .flatten()
        .and_then(|v| v.as_int())
        .unwrap_or(0)
}

/// Read a BitSet's packed words (`$bits` = `long[]`, 64 bits per word).
/// The `$bits` array itself plus its length — for the SCANNING accessors, which touch a few
/// words and do not need the whole set copied.
///
/// ⭐`bitset_words` materialises every word into a fresh `Vec<u64>` on every call. For the bulk
/// operations that is the right shape; for `nextClearBit`/`nextSetBit`/`get` it means a 64-word
/// allocation and copy to read one word. `bench/SeamCost.java` prices `BitSet.nextClearBit` at
/// **1,780 ns against HotSpot's 0-4**, and the JIT-callback census puts the BitSet family at
/// 5.83% of the boot's 64,088,346 crossings.
/// Does this value's class prove that `equals` IS reference identity? Reuses the bytecode rule
/// built for map keys — same question, same evidence.
pub(crate) fn objects_identity_equals(vm: &mut Vm, v: &Value) -> bool {
    match v {
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            crate::map::keys::identity_verdict_cid(vm, crate::nextgen::arena_class_id(*p))
        }
        Value::Ref(h) => match vm.heap.object_class_word(*h) {
            Some(w) => crate::map::keys::identity_verdict_word(vm, w, *h),
            None => false,
        },
        _ => false,
    }
}

fn bitset_array(vm: &Vm, recv: &Value) -> Option<(u32, usize)> {
    match vm.synth_get_ro(recv, "$bits") {
        Value::Array(a) => Some((a, vm.heap.array_len(a).unwrap_or(0))),
        _ => None,
    }
}

#[inline]
fn bitset_word(vm: &Vm, arr: u32, i: usize) -> u64 {
    vm.heap.array_get(arr, i).and_then(|v| v.as_long()).unwrap_or(0) as u64
}

fn bitset_words(vm: &Vm, recv: &Value) -> Vec<u64> {
    match vm.synth_get_ro(recv, "$bits") {
        Value::Array(a) => (0..vm.heap.array_len(a).unwrap_or(0))
            .map(|i| vm.heap.array_get(a, i).and_then(|v| v.as_long()).unwrap_or(0) as u64)
            .collect(),
        _ => Vec::new(),
    }
}

/// java.lang.Math / StrictMath. Integer ops keep their type (by return
/// `Math.min(double,double)` — Java semantics, NOT Rust's `f64::min`. The crucial
/// difference is NaN: Java PROPAGATES it (`min(NaN,x)==NaN`) whereas Rust returns
/// the non-NaN operand. MC's pathfinding/physics relies on a `0/0=NaN` flowing
/// through min/max so an `isNaN` guard catches it; dropping the NaN let a bad
/// coordinate escape → entities (and the player) blew up to ±3e7. Also matches
/// Java's signed-zero preference (min prefers -0.0).
fn java_fmin(a: f64, b: f64) -> f64 {
    if a.is_nan() { return a; }
    if a == 0.0 && b == 0.0 && b.is_sign_negative() { return b; }
    if a <= b { a } else { b }
}

/// `Double.compare` / `Double.compareTo` — a TOTAL order, unlike `<`/`>`, which
/// call -0.0 and 0.0 equal and every NaN comparison false. The JDK separates them
/// on the BITS after canonicalising NaN, which is order-preserving; f32 widens to
/// f64 exactly, so this serves `Float.compare` too. Only the tie is decided by
/// bits, so the sign flip that bit order has on negative values never shows.
fn java_dcmp(a: f64, b: f64) -> i32 {
    if a < b { return -1; }
    if a > b { return 1; }
    let bits = |x: f64| -> i64 {
        if x.is_nan() { 0x7FF8_0000_0000_0000u64 as i64 } else { x.to_bits() as i64 }
    };
    let (ab, bb) = (bits(a), bits(b));
    (ab > bb) as i32 - (ab < bb) as i32
}

/// `Math.max(double,double)` — Java semantics (propagates NaN; max prefers +0.0).
fn java_fmax(a: f64, b: f64) -> f64 {
    if a.is_nan() { return a; }
    if a == 0.0 && b == 0.0 && a.is_sign_negative() { return b; }
    if a >= b { a } else { b }
}

fn epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
mod cascade_owners;
pub(crate) use cascade_owners::*;
mod cascade_table2;
pub(crate) use cascade_table2::*;
mod cascade_table;
pub(crate) use cascade_table::*;
pub(crate) mod cascade;
pub(crate) use cascade::*;
mod format;
pub(crate) use format::*;
mod strings;
pub(crate) use strings::*;
mod io;
pub(crate) use io::*;
mod crypto;
pub(crate) use crypto::*;
mod math;
pub(crate) use math::*;
mod buffers;
pub(crate) use buffers::*;
mod program;
pub(crate) use program::{
    invoke as invoke_program, invoke_cells as invoke_program_cells, program_for, program_for_binding,
    program_for_class, NativeProgram, NativeProgramClass,
};
mod time;
pub(crate) use time::*;
mod misc;
pub(crate) use misc::*;
pub mod entry;
pub(crate) use entry::*;
mod text;
pub(crate) use text::*;
mod streams;
pub(crate) use streams::*;
mod security;
pub(crate) use security::*;
mod values;
pub(crate) use values::*;
mod streamio;
pub(crate) use streamio::*;
mod gatestat;
pub(crate) use gatestat::*;
mod buffers_glue;
pub(crate) use buffers_glue::*;
/// BitSet word scans (`buffers::bitset_dispatch` calls into it by path, so no glob re-export —
/// the names here would collide with this module's own `bitset_word`/`bitset_words` helpers).
mod bitset;
