//! System properties, statics, throwables, Objects and regex.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

pub(crate) fn property(key: Option<&Value>) -> Option<String> {
    // Accept both Value::Str (interpreter) and Value::Obj ArenaString (compiled callers);
    // a Str-only match returned None for every compiled System.getProperty(key) call.
    let key: String = match key {
        Some(Value::Str(s)) => s.to_string(),
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) if crate::nextgen::is_arena_string(*p) => {
            String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)).into_owned()
        }
        _ => return None,
    };
    let key = key.as_str();
    if let Some(v) = USER_PROPS.get().and_then(|m| m.get(key)) {
        return Some(v.clone());
    }
    let v = match key {
        "file.separator" => std::path::MAIN_SEPARATOR.to_string(),
        "path.separator" => if cfg!(windows) { ";" } else { ":" }.to_string(),
        "line.separator" => "\n".to_string(),
        "java.version" => "22.0.2".to_string(),
        "java.specification.version" => "22".to_string(),
        "java.class.version" => "66.0".to_string(),
        "java.vm.name" => "Rava".to_string(),
        "java.vm.vendor" | "java.vendor" => "Rava Project".to_string(),
        "java.vm.version" | "java.runtime.version" => "22.0.2+0".to_string(),
        "java.runtime.name" => "Rava Runtime Environment".to_string(),
        "java.vm.specification.version" => "22".to_string(),
        "java.vendor.url" => "https://github.com/".to_string(),
        "os.name" => if cfg!(windows) { "Windows 11" } else { std::env::consts::OS }.to_string(),
        "os.arch" => std::env::consts::ARCH.to_string(),
        "os.version" => "10.0".to_string(),
        "user.dir" => std::env::current_dir().ok()?.to_string_lossy().into_owned(),
        "user.name" => std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "user".into()),
        "user.home" => std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into()),
        "java.home" => std::env::var("JAVA_HOME").unwrap_or_else(|_| ".".into()),
        "java.io.tmpdir" => std::env::temp_dir().to_string_lossy().into_owned(),
        "sun.jnu.encoding" | "file.encoding" | "native.encoding" | "stdout.encoding" | "stderr.encoding" => "UTF-8".to_string(),
        // Force netty onto heap byte[] buffers with safe access. The engine can't
        // back sun.misc.Unsafe direct/off-heap memory, so pooled/direct ByteBufs
        // wrote nowhere and flushed as zeros (the status response came out as 128
        // zero bytes). Heap + unpooled keeps every ByteBuf an addressable byte[].
        "io.netty.noUnsafe" | "io.netty.noPreferDirect" => "true".to_string(),
        "io.netty.allocator.type" => "unpooled".to_string(),
        _ => return None,
    };
    Some(v)
}

// ────────────────────────────────────────────────────────────────────────────────────────────
// ONE WORD FOR EVERY PROCESS CONSTANT ON THE `Objects.equals` PATH
//
// Fold process constants into one word at the equality arm's natural entry, then thread it
// through the helpers. `requireNonNull`/`isNull`/`hashCode` do not read these constants and must
// not pay for it.
//
// Every bit is the SAME predicate read from the SAME accessor, so an armed run behaves exactly as
// it did — this is `natives::entry::entry_gates()`'s shape, applied one layer further in.
// ────────────────────────────────────────────────────────────────────────────────────────────

/// `RAVA_NO_FAST_MISC` is UNSET (and the block switch is off): everything this file pair adds is
/// armed — the JDK-constant table, the `Matcher` field hoist, the per-class equality cache.
const MG_FAST_MISC: u8 = 1 << 0;
/// `RAVA_NO_EQUALS_KIND_FASTPATH` is UNSET: `Objects.equals` may decide by `EqualsKind` instead of
/// always re-entering the guest.
const MG_EK_FAST: u8 = 1 << 1;
/// `RAVA_EQ_PEEK` is SET (opt-in): the `EqualsKind::Value` arm only compares content keys that
/// ALREADY EXIST, instead of deriving on a miss. See the measurement at that arm.
const MG_EQ_PEEK: u8 = 1 << 2;
/// `RAVA_NO_EQ_CHEAP_KIND` is UNSET: `eq_kind_of` peeks the memo through a BORROWED class name
/// rather than building a `String` first.
const MG_EQ_CHEAP: u8 = 1 << 4;
/// KILL SWITCH `RAVA_NO_FAST_NATSHAPE=1` (or `RAVA_NO_EMIT_MORE`): every folded constant is read
/// back at its own call site, from its own static, exactly as it was before this round. The word
/// is still built — it has to be, to carry this bit — so the switch restores the READS, which is
/// what is being measured, not the answers, which never differed.
const MG_LEGACY: u8 = 1 << 5;

/// The `Objects.equals` path's process constants, decided once per process.
#[inline]
fn misc_gates() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u8;
        let block_off = crate::flags::emit_more_disabled();
        if std::env::var_os("RAVA_NO_FAST_MISC").is_none() && !block_off {
            w |= MG_FAST_MISC;
        }
        if std::env::var_os("RAVA_NO_EQUALS_KIND_FASTPATH").is_none() {
            w |= MG_EK_FAST;
        }
        if std::env::var_os("RAVA_EQ_PEEK").is_some() {
            w |= MG_EQ_PEEK;
        }
        if std::env::var_os("RAVA_NO_EQ_CHEAP_KIND").is_none() {
            w |= MG_EQ_CHEAP;
        }
        // Falls together with the other emit-more switches, as every gate in this tree does.
        if std::env::var_os("RAVA_NO_FAST_NATSHAPE").is_some() || block_off {
            w |= MG_LEGACY;
        }
        w
    })
}

/// One folded bit, or — under the kill switch — the historic per-site acquire load that bit
/// replaced. `legacy` is a `fn` pointer so each call site passes its OWN original closure and the
/// restored behaviour is the original code, not a re-derivation of it; it inlines to nothing on
/// the default path, where only the bit test survives.
#[inline]
fn mg(g: u8, bit: u8, legacy: fn() -> bool) -> bool {
    if g & MG_LEGACY != 0 {
        return legacy();
    }
    g & bit != 0
}

/// ONE SWITCH FOR EVERYTHING THIS FILE PAIR ADDS (`RAVA_NO_FAST_MISC=1`), so the whole block
/// prices as a single same-binary flag A/B rather than as five separate ones. Also consults the
/// project-wide `RAVA_NO_EMIT_MORE` gate, per the house rule.
///
/// Read once — `var_os` on a native path would cost more than anything the flag guards. It is now
/// a bit of `misc_gates()` rather than a static of its own, so the two `Objects.equals` sites that
/// used to read it independently read the same word the rest of the arm already holds. The
/// signature is unchanged for `math.rs`, which is not this round's file.
pub(crate) fn fast_misc() -> bool {
    misc_gates() & MG_FAST_MISC != 0
}

/// The static `fast_misc` used to own, kept alive for exactly one purpose: under `MG_LEGACY` the
/// two `Objects.equals` sites that now bit-test the word read THIS instead, so the kill switch
/// restores the original two acquire loads and the A/B measures the reads rather than the answers.
fn fast_misc_legacy() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_MISC").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// One interned JDK constant: the `(owner, name)` that names it, the class the arm actually
/// ALLOCATED for it (`StandardCharsets.UTF_8` is a `java/nio/charset/Charset`, not a
/// `StandardCharsets`), the `$name` text it stamped, and the value itself.
///
/// `cls`/`tag` exist for one reason and it is not speed: this table is NOT a GC root, so a
/// cached handle can be swept and its slot reused by an unrelated object. Re-checking that the
/// handle is alive, still of the class the arm built, and still carrying the same `$name` makes
/// a reused slot impossible to mistake for the constant — the only object that passes all three
/// is one indistinguishable from the one we would have allocated.
struct StaticConst {
    owner: Box<str>,
    name: Box<str>,
    cls: Box<str>,
    tag: Box<str>,
    value: Value,
}

thread_local! {
    /// `(owner, name)` -> the JDK constant, keyed by a fold of the two names and VERIFIED
    /// against the stored copies, so a hash collision cannot serve the wrong constant.
    ///
    /// Bounded by the number of distinct native statics a process ever reads — a few dozen
    /// (`StandardCharsets`, `ByteOrder`, `TimeUnit`, `Locale`, `ChronoUnit`, `RoundingMode`,
    /// the option markers). At ~80 bytes of entry plus two short names that is **well under
    /// 4 KB**, and it is INDEXED by an integer rather than scanned, so a lookup touches one
    /// line — this is a process-lifetime memo in the shape of `equality_is_identity`'s, not an
    /// L1 cache whose whole footprint has to stay resident.
    static STATIC_CONSTS: std::cell::RefCell<crate::FastMap<u64, StaticConst>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// Fold `(owner, name)` to a table key. Verified by content on every hit, so this only has to
/// spread, not to be collision-free.
fn static_const_key(owner: &str, name: &str) -> u64 {
    use std::hash::Hasher;
    // The crate's own word-at-a-time fold, not SipHash: these are the engine's names, never an
    // attacker's, and the two together are ~40 bytes = five rounds.
    let mut h = crate::FxHasher::default();
    h.write(owner.as_bytes());
    h.write(name.as_bytes());
    h.finish()
}

/// Is a cached entry still the constant it was built as? See `StaticConst` for why all three
/// tests are needed and what each one closes.
fn static_const_intact(heap: &crate::heap::Heap, e: &StaticConst) -> bool {
    match &e.value {
        Value::Ref(h) => {
            let h = *h;
            heap.is_ref_alive(h)
                && heap.class_of(h) == Some(&*e.cls)
                && matches!(heap.get_field_syn(h, crate::heap::Syn::Name), Value::Str(s) if s.as_str() == &*e.tag)
        }
        // A `Value::Str`/`Value::Int` constant (the `java.io.File` separators) holds no handle
        // and cannot go stale.
        _ => true,
    }
}

/// ⭐**A JDK CONSTANT IS BORN ONCE PER PROCESS, NOT ONCE PER READ.**
///
/// `get_static` is the `getstatic` path for a JDK class with no bytecode in the jar, and almost
/// every arm below ends in `vm.heap.alloc(owner)` plus one to three by-name field writes. A
/// BIRTH is the most expensive thing this engine does — `docs/per-op-price-list.md` prices
/// `new Object` at 344 ns against HotSpot's ~0 — and the interpreter's per-site `getstatic`
/// cache cannot amortise it here: `putstatic` bumps `vm.static_gen`, which invalidates EVERY
/// site at once, and the first 53% of the boot is class loading and static initialisation, i.e.
/// a continuous stream of `putstatic`. So `StandardCharsets.UTF_8` is re-decided through ~25
/// owner-name comparisons and re-allocated, over and over, for a value that is a compile-time
/// constant of the JDK.
///
/// It also makes these constants what the JVM says they are. `StandardCharsets.UTF_8 ==
/// StandardCharsets.UTF_8`, `unit == ChronoUnit.SECONDS`, `order == ByteOrder.BIG_ENDIAN` and
/// `locale == Locale.ROOT` are all true on HotSpot and were all FALSE here, because every read
/// handed out a fresh handle. Sharing one object per constant is the JDK's own contract for
/// these fields (they are `static final` singletons), so this narrows a divergence rather than
/// opening one.
///
/// Removing it (`RAVA_NO_FAST_MISC=1`) restores the allocate-per-read behaviour exactly.
pub(crate) fn get_static(vm: &mut Vm, owner: &str, name: &str) -> Option<Value> {
    if !fast_misc() {
        return get_static_uncached(vm, owner, name);
    }
    let key = static_const_key(owner, name);
    let hit = STATIC_CONSTS.with(|m| {
        let t = m.borrow();
        match t.get(&key) {
            Some(e)
                if &*e.owner == owner
                    && &*e.name == name
                    && static_const_intact(&vm.heap, e) =>
            {
                Some(e.value.clone())
            }
            _ => None,
        }
    });
    if let Some(v) = hit {
        return Some(v);
    }
    let value = get_static_uncached(vm, owner, name)?;
    // Only a constant whose identity this table can PROVE on a later read is stored. That is
    // every `$name`-stamped arm (the charsets, the enums, the option markers) plus the handle-free
    // ones; `CASE_INSENSITIVE_ORDER` (tagged `$cmp`, not `$name`) is deliberately left to
    // re-allocate rather than given a weaker guard. `Boolean.TRUE`/`.FALSE` and the `TYPE` class
    // mirrors already come from `vm.bool_cache`/`vm.class_objects`, which ARE GC roots, so they
    // are singletons without this table's help and lose nothing by falling through.
    let storable: Option<(Box<str>, Box<str>)> = match &value {
        Value::Ref(h) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Name) {
            Value::Str(tag) if !tag.is_empty() => {
                vm.heap.class_of(*h).map(|c| (Box::from(c), Box::from(&*tag)))
            }
            _ => None,
        },
        Value::Str(_) | Value::Int(_) => Some((Box::from(""), Box::from(""))),
        _ => None,
    };
    if let Some((cls, tag)) = storable {
        STATIC_CONSTS.with(|m| {
            m.borrow_mut().insert(
                key,
                StaticConst {
                    owner: Box::from(owner),
                    name: Box::from(name),
                    cls,
                    tag,
                    value: value.clone(),
                },
            );
        });
    }
    Some(value)
}

/// Native static fields for JDK classes with no bytecode in the jar.
fn get_static_uncached(vm: &mut Vm, owner: &str, name: &str) -> Option<Value> {
    // java.io.File path separators — we use '/' uniformly (our paths are POSIX).
    if owner == "java/io/File" {
        return match name {
            "separatorChar" => Some(Value::Int('/' as i32)),
            "separator" => Some(Value::Str("/".into())),
            "pathSeparatorChar" => Some(Value::Int(';' as i32)),
            "pathSeparator" => Some(Value::Str(";".into())),
            _ => None,
        };
    }
    // Enum option constants (CopyOption/OpenOption/LinkOption/...) — opaque
    // markers; the Files natives don't vary on them.
    if owner.ends_with("Option") {
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        return Some(Value::Ref(h));
    }
    if owner == "java/time/format/DateTimeFormatter" || owner == "java/time/ZoneOffset" || owner == "java/time/ZoneId" {
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        return Some(Value::Ref(h));
    }
    // Collector.Characteristics (CONCURRENT/UNORDERED/IDENTITY_FINISH) — opaque
    // enum markers; our collector natives don't vary on them.
    if owner == "java/util/stream/Collector$Characteristics" {
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        return Some(Value::Ref(h));
    }
    // Boolean.TRUE / Boolean.FALSE → the singleton boxed Boolean objects, so
    // identity and instanceof match values produced by Boolean.valueOf.
    if owner == "java/lang/Boolean" && (name == "TRUE" || name == "FALSE") {
        return Some(vm.boolean_obj(name == "TRUE"));
    }
    // Primitive wrapper TYPE fields → Class objects naming the primitive.
    if name == "TYPE" {
        let prim = match owner {
            "java/lang/Void" => Some("void"),
            "java/lang/Integer" => Some("int"),
            "java/lang/Long" => Some("long"),
            "java/lang/Boolean" => Some("boolean"),
            "java/lang/Character" => Some("char"),
            "java/lang/Byte" => Some("byte"),
            "java/lang/Short" => Some("short"),
            "java/lang/Float" => Some("float"),
            "java/lang/Double" => Some("double"),
            _ => None,
        };
        if let Some(p) = prim {
            return Some(vm.make_class(p));
        }
    }
    // `String.CASE_INSENSITIVE_ORDER` — a real static comparator, and every
    // case-insensitive sort in the JDK's own code reaches for it. Absent, so the read
    // failed; served as the tree's `$cmp`-tagged comparator so `Vm::compare` decides it
    // without a guest frame.
    if owner == "java/lang/String" && name == "CASE_INSENSITIVE_ORDER" {
        let h = vm.heap.alloc("java/util/Comparator");
        vm.heap.put_field(h, "$cmp", Value::Str("ciorder".into()));
        return Some(Value::Ref(h));
    }
    if owner == "java/util/logging/Level" {
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        return Some(Value::Ref(h));
    }
    // java.nio.file.FileVisitResult — Files.walkFileTree decodes the visitor's
    // returned constant by ordinal (CONTINUE=0/TERMINATE=1/SKIP_SUBTREE=2/
    // SKIP_SIBLINGS=3), so these statics must carry $enumOrd.
    if owner == "java/nio/file/FileVisitResult" {
        let ord = match name { "TERMINATE" => 1, "SKIP_SUBTREE" => 2, "SKIP_SIBLINGS" => 3, _ => 0 };
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        vm.heap.put_field(h, "$enumName", Value::Str(name.into()));
        vm.heap.put_field(h, "$enumOrd", Value::Int(ord));
        return Some(Value::Ref(h));
    }
    // java.net.Proxy.NO_PROXY / Proxy.Type.* — opaque markers; our network
    // natives don't route through a real proxy, so any handle passed along works.
    if owner == "java/net/Proxy" || owner == "java/net/Proxy$Type" {
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        return Some(Value::Ref(h));
    }
    if owner == "java/nio/ByteOrder" || owner == "java/nio/charset/CodingErrorAction" {
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        return Some(Value::Ref(h));
    }
    if owner == "java/math/RoundingMode" || owner == "java/time/temporal/ChronoUnit"
        || owner == "java/time/temporal/ChronoField" || owner == "java/time/DayOfWeek"
        || owner == "java/time/Month" {
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        vm.heap.put_field(h, "$enumName", Value::Str(name.into()));
        // Real ordinal for RoundingMode (used as an array index); others default 0.
        if let Some(ord) = ROUNDING_MODES.iter().position(|m| *m == name) {
            vm.heap.put_field(h, "$enumOrd", Value::Int(ord as i32));
        }
        return Some(Value::Ref(h));
    }
    if owner == "java/util/concurrent/TimeUnit" {
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        vm.heap.put_field(h, "$enumName", Value::Str(name.into()));
        return Some(Value::Ref(h));
    }
    if owner == "java/util/Locale" {
        let h = vm.heap.alloc("java/util/Locale");
        vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(name.into()));
        return Some(Value::Ref(h));
    }
    match owner {
        "java/nio/charset/StandardCharsets" => {
            let cs = match name {
                "UTF_8" => "UTF-8",
                "US_ASCII" => "US-ASCII",
                "ISO_8859_1" => "ISO-8859-1",
                "UTF_16" => "UTF-16",
                "UTF_16BE" => "UTF-16BE",
                "UTF_16LE" => "UTF-16LE",
                _ => return None,
            };
            let h = vm.heap.alloc("java/nio/charset/Charset");
            vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(cs.into()));
            Some(Value::Ref(h))
        }
        _ => None,
    }
}

/// `Throwable.toString()` as the JDK defines it: the RUNTIME class's binary name,
/// then ": " + detailMessage only when there is one. Used by the `toString` arm,
/// by the `Throwable(Throwable)` constructor (whose message IS `cause.toString()`)
/// and by `initCause`'s IllegalStateException text — one spelling, three callers.
fn throwable_to_string(vm: &Vm, h: u32) -> String {
    // The RECEIVER's class, not the invoke's declared owner: a guest
    // `class MyErr extends RuntimeException` inherits this method, and HotSpot
    // prints `MyErr`, not `java.lang.RuntimeException`.
    let cls = vm.heap.class_of(h).unwrap_or("java/lang/Throwable").replace('/', ".");
    match vm.heap.get_field(h, "$message") {
        Value::Str(s) => format!("{cls}: {s}"),
        _ => cls,
    }
}

/// The legacy one-int constructors build their message from the offending index
/// (JDK: `ArrayIndexOutOfBoundsException(int)` → "Array index out of range: -1").
fn index_ctor_prefix(owner: &str) -> Option<&'static str> {
    Some(match owner {
        "java/lang/ArrayIndexOutOfBoundsException" => "Array index out of range: ",
        "java/lang/StringIndexOutOfBoundsException" => "String index out of range: ",
        "java/lang/IndexOutOfBoundsException" => "Index out of range: ",
        _ => return None,
    })
}

/// Throwable behaviour shared by every exception/error type: a `<init>` that
/// stores the message/cause, and the common accessors. Returns the optional
/// return value when handled.
pub(crate) fn throwable(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    let recv = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Ok(None),
    };
    Ok(match name {
        "<init>" => {
            // CAPTURE HERE. Java captures at construction — `Throwable()` calls
            // `fillInStackTrace()` — and by the time anyone asks for the trace the frames
            // are gone, so there is nowhere else it could happen.
            crate::stacktrace::capture(vm, recv);
            // POSITIONAL, keyed on the declared parameter types. The old rule was
            // "first String argument anywhere = message, first Ref = cause", which
            // cannot tell `new IOException(null, cause)` from `new IOException(msg)`
            // — the runtime Values are indistinguishable, the descriptors are not.
            let params = crate::argcall::param_types(descriptor);
            for (i, ty) in params.iter().enumerate() {
                let arg = args.get(i + 1);
                match *ty {
                    "Ljava/lang/String;" => {
                        // `str_of`, not a `Value::Str` pattern: a message that
                        // arrives as a heap or ARENA string is a `Ref`/`Obj` whose
                        // text lives in `$str`/inline bytes, and matching the
                        // variant dropped it. Null stays null — `str_of` would
                        // render it as the four characters "null".
                        if !matches!(arg, None | Some(Value::Null)) {
                            if let Some(s) = str_of(vm, arg) {
                                vm.heap.put_field(recv, "$message", Value::Str(s.into()));
                            }
                        }
                    }
                    "Ljava/lang/Throwable;" => {
                        if let Some(c) = arg {
                            vm.heap.put_field(recv, "$cause", c.clone());
                        }
                        // A DECLARED cause parameter locks `initCause` out even when
                        // the argument is null: the JDK's "unset" sentinel is the
                        // cause field still pointing at `this`, which only the
                        // no-cause constructors leave in place.
                        vm.heap.put_field(recv, "$causeSet", Value::Int(1));
                    }
                    _ => {}
                }
            }
            // `Throwable(Throwable cause)` has no message parameter; its message is
            // `cause.toString()`.
            if params.len() == 1 && params[0] == "Ljava/lang/Throwable;" {
                if let Some(Value::Ref(c)) = args.get(1) {
                    let text = throwable_to_string(vm, *c);
                    vm.heap.put_field(recv, "$message", Value::Str(text.into()));
                }
            }
            if params.len() == 1 && params[0] == "I" {
                if let (Some(prefix), Some(Value::Int(n))) = (index_ctor_prefix(owner), args.get(1)) {
                    vm.heap.put_field(recv, "$message", Value::Str(format!("{prefix}{n}").into()));
                }
            }
            // Non-canonical shapes (`AssertionError(Object)`, `(String,int)` …) keep
            // the old first-String rule, so nothing that used to get a message loses
            // one.
            if !matches!(vm.heap.get_field(recv, "$message"), Value::Str(_)) {
                if let Some(Value::Str(s)) = args.iter().skip(1).find(|a| matches!(a, Value::Str(_))) {
                    vm.heap.put_field(recv, "$message", Value::Str(s.clone()));
                }
            }
            Some(None)
        }
        "getMessage" | "getLocalizedMessage" => Some(Some(vm.heap.get_field(recv, "$message"))),
        "getCause" => Some(Some(match vm.heap.get_field(recv, "$cause") {
            Value::Null => Value::Null,
            c => c,
        })),
        "toString" => Some(Some(Value::Str(throwable_to_string(vm, recv).into()))),
        "printStackTrace" => {
            let line = throwable_to_string(vm, recv);
            // printStackTrace(PrintWriter/PrintStream) writes into that target.
            match args.get(1) {
                Some(Value::Ref(w)) => { let t = writer_target(vm, *w); writer_append(vm, t, &format!("{line}\n")); }
                _ => eprintln!("{line}"),
            }
            Some(None)
        }
        "fillInStackTrace" => {
            // Java's contract: this RE-captures at the point it is called, and returns
            // `this` so `throw e.fillInStackTrace()` chains.
            crate::stacktrace::capture(vm, recv);
            Some(Some(Value::Ref(recv)))
        }
        "initCause" => {
            // JDK contract: the cause may be set ONCE, and only if no constructor
            // supplied one — a second call is an IllegalStateException naming the
            // cause already in place. Returning `this` unconditionally without
            // storing anything left `getCause()` null after a successful
            // `initCause`, so a wrapper exception lost its whole chain.
            if matches!(vm.heap.get_field(recv, "$causeSet"), Value::Int(1)) {
                // The JDK names the cause you TRIED to set, not the one already
                // there: `"Can't overwrite cause with " + Objects.toString(cause,
                // "a null")`.
                let attempted = match args.get(1) {
                    Some(Value::Ref(c)) => throwable_to_string(vm, *c),
                    _ => "a null".into(),
                };
                let e = vm.make_exception(
                    "java/lang/IllegalStateException",
                    &format!("Can't overwrite cause with {attempted}"),
                );
                return Err(VmError::Throw(e));
            }
            let cause = args.get(1).cloned().unwrap_or(Value::Null);
            vm.heap.put_field(recv, "$cause", cause);
            vm.heap.put_field(recv, "$causeSet", Value::Int(1));
            Some(Some(Value::Ref(recv)))
        }
        "setStackTrace" => Some(None),
        // Suppression is what try-with-resources reports a failing close() through:
        // javac lowers the resource block into `primary.addSuppressed(closeEx)`, so
        // a no-op `addSuppressed` DELETED the close failure from the guest's view,
        // and `getSuppressed` was not served at all — the engine raised
        // UnsatisfiedLinkError from inside an ordinary catch block.
        "addSuppressed" => {
            let exc = args.get(1).cloned().unwrap_or(Value::Null);
            match exc {
                Value::Null => {
                    let e = vm.make_exception(
                        "java/lang/NullPointerException",
                        "Cannot suppress a null exception.",
                    );
                    return Err(VmError::Throw(e));
                }
                Value::Ref(h) if h == recv => {
                    let e = vm.make_exception(
                        "java/lang/IllegalArgumentException",
                        "Self-suppression not permitted",
                    );
                    return Err(VmError::Throw(e));
                }
                _ => {}
            }
            let mut list = suppressed_of(vm, recv);
            list.push(exc);
            let h = vm.heap.alloc_values("Ljava/lang/Throwable;", list);
            vm.heap.put_field(recv, "$suppressed", Value::Array(h));
            Some(None)
        }
        // A fresh array every call: the JDK returns a CLONE, so a guest that nulls
        // an element of the result must not corrupt the exception.
        "getSuppressed" => {
            let list = suppressed_of(vm, recv);
            Some(Some(Value::Array(vm.heap.alloc_values("Ljava/lang/Throwable;", list))))
        }
        "getStackTrace" => Some(Some(crate::stacktrace::elements(vm, recv))),
        // A throwable with no captured frames answers 0, which is what an empty array
        // meant before — so a caller that only asks the length is unchanged.
        "getStackTraceDepth" => Some(Some(Value::Int(
            match crate::stacktrace::elements(vm, recv) {
                Value::Array(a) => vm.heap.array_len(a).unwrap_or(0) as i32,
                _ => 0,
            },
        ))),
        _ => None,
    })
}

/// The receiver's suppressed list as a plain Vec (empty when none were added).
fn suppressed_of(vm: &Vm, recv: u32) -> Vec<Value> {
    match vm.heap.get_field(recv, "$suppressed") {
        Value::Array(h) => {
            let len = vm.heap.array_len(h).unwrap_or(0);
            (0..len).map(|i| vm.heap.array_get(h, i).unwrap_or(Value::Null)).collect()
        }
        _ => Vec::new(),
    }
}

/// java.util.regex Pattern + Matcher, backed by the `regex` crate.
/// THE THROWING WRAPPER — see `uuid_dispatch`. A malformed pattern compiled to nothing
/// and matched nothing instead of raising, and `group()` before a successful match
/// returned the input instead of complaining.
pub(crate) fn regex_dispatch(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    // ⛔NO PatternSyntaxException HERE, AND THE BOOT IS WHY. The first version raised it
    // whenever `cached_regex` returned `None`, which conflates two different things:
    // a MALFORMED pattern, and a pattern this engine's regex crate does not implement.
    // log4j compiles
    //
    //     (^log4j2?[-._/]?|^org\.apache\.logging\.log4j\.)|(?=AsyncLogger(Config)?\.)
    //
    // which is valid Java and uses a LOOKAHEAD the Rust crate has no support for — so
    // the check threw on a correct pattern and the server died before its first log line.
    // Silently failing to match is wrong for a malformed pattern and right for an
    // unsupported one, and this engine cannot tell them apart, so it keeps the behaviour
    // that leaves working programs working. `bench/PlatSem.java` records the gap.
    if owner == "java/util/regex/Matcher" && name == "group" {
        if let Some(Value::Ref(h)) = args.first() {
            if vm.heap.get_field(*h, "$mend").as_int().is_none() {
                let e = vm.make_exception("java/lang/IllegalStateException", "No match found");
                return Err(VmError::Throw(e));
            }
        }
    }
    Ok(regex_dispatch_inner(vm, owner, name, args))
}

/// `$g0`..`$g9` as literals. `format!("$g{n}")` built a `String` to NAME a field — once per
/// capture group per `find`, and again per `group(n)` read — so a two-group scan over a datapack
/// line allocated three times per match just to look three fields up. **10 × 16 B = 160 bytes**
/// of static slices. A group past 9 still formats; nothing that common gets there.
const GROUP_FIELDS: [&str; 10] =
    ["$g0", "$g1", "$g2", "$g3", "$g4", "$g5", "$g6", "$g7", "$g8", "$g9"];

/// The synthetic field name for capture group `n`, borrowed when it is one of the first ten.
fn group_field(n: i32) -> std::borrow::Cow<'static, str> {
    match usize::try_from(n).ok().and_then(|i| GROUP_FIELDS.get(i)) {
        Some(s) => std::borrow::Cow::Borrowed(s),
        None => std::borrow::Cow::Owned(format!("$g{n}")),
    }
}

/// The Matcher methods whose whole answer is a synthetic field on the receiver: no pattern, no
/// subject, no compiled regex. `None` = not one of them, and the caller runs the full body.
///
/// Called from TWO places on purpose (see the hoist in `regex_dispatch_inner`) so the fast order
/// and the flag-off order share one implementation and cannot drift.
///
/// ⚠One behavioural difference is deliberate and is an improvement: reached from the hoist,
/// `group` answers even when the pattern does not compile under the `regex` crate, where the old
/// order returned `None` from the `?` on `cached_regex` and the engine raised
/// UnsatisfiedLinkError. `regex_dispatch`'s wrapper already raises the JDK's
/// `IllegalStateException("No match found")` when `$mend` is unset, which is the check that
/// actually guards this method.
fn matcher_field_arm(
    vm: &mut Vm,
    recv: u32,
    name: &str,
    args: &[Value],
) -> Option<Option<Value>> {
    match name {
        "start" => Some(Some(vm.heap.get_field(recv, "$mstart"))),
        "end" => Some(Some(vm.heap.get_field(recv, "$mend"))),
        "reset" => {
            vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Int(0));
            Some(Some(Value::Ref(recv)))
        }
        "group" => Some(Some(match args.get(1) {
            Some(Value::Str(gname)) => vm.heap.get_field(recv, &format!("$gn_{gname}")),
            other => {
                let n = other.and_then(Value::as_int).unwrap_or(0);
                vm.heap.get_field(recv, &group_field(n))
            }
        })),
        _ => None,
    }
}

fn regex_dispatch_inner(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Option<Option<Value>> {
    if owner == "java/util/regex/Pattern" {
        match name {
            "compile" => {
                let pat = str_of(vm, args.first()).unwrap_or_default();
                let h = vm.heap.alloc("java/util/regex/Pattern");
                vm.heap.put_field(h, "$regex", Value::Str(pat.into()));
                return Some(Some(Value::Ref(h)));
            }
            "matches" => {
                let pat = str_of(vm, args.first()).unwrap_or_default();
                let input = str_of(vm, args.get(1)).unwrap_or_default();
                let m = cached_regex(&format!("^(?:{pat})$")).map(|re| re.is_match(&input)).unwrap_or(false);
                return Some(Some(Value::Int(m as i32)));
            }
            // `Pattern.quote` returns a \\Q...\\E literal block, NOT a per-character escape.
            // Guests compare and concatenate the result, so the shape is the answer.
            "quote" => {
                let raw = str_of(vm, args.first()).unwrap_or_default();
                // A literal \\E inside has to close and reopen the block, as the JDK does.
                let body = raw.replace("\\E", "\\E\\\\E\\Q");
                return Some(Some(Value::Str(format!("\\Q{body}\\E").into())));
            }
            "pattern" | "toString" => return Some(Some(Value::Str(str_field_of(vm, args.first(), "$regex").into()))),
            // Pattern.split(CharSequence[, limit]) — same regex-split semantics as
            // String.split, but the pattern lives on the receiver's $regex field.
            // (SimpleCommandMap parses every player command through this.)
            "split" => {
                let pat = str_field_of(vm, args.first(), "$regex");
                let input = str_of(vm, args.get(1)).unwrap_or_default();
                let limit = args.get(2).and_then(Value::as_int).unwrap_or(0);
                let parts: Vec<Value> = split_regex(&input, &pat, limit)
                    .into_iter()
                    .map(|p| Value::Str(p.into()))
                    .collect();
                let h = vm.heap.alloc_values("Ljava/lang/String;", parts);
                return Some(Some(Value::Array(h)));
            }
            "matcher" => {
                let regex = str_field_of(vm, args.first(), "$regex");
                let input = str_of(vm, args.get(1)).unwrap_or_default();
                let m = vm.heap.alloc("java/util/regex/Matcher");
                vm.heap.put_field(m, "$regex", Value::Str(regex.into()));
                vm.heap.put_field(m, "$input", Value::Str(input.into()));
                vm.heap.put_field_syn(m, crate::heap::Syn::Pos, Value::Int(0));
                return Some(Some(Value::Ref(m)));
            }
            _ => return None,
        }
    }
    // Matcher
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return None };
    // ⭐**THE ARMS THAT ONLY READ A FIELD MUST NOT COPY THE SUBJECT — THE OLD ORDER WAS
    // O(input) PER CALL AND THEREFORE O(n²) OVER A SCAN LOOP.** Every Matcher method, including
    // `start`/`end`/`group`/`reset`, first materialised BOTH the pattern and the WHOLE INPUT as
    // fresh `String`s (`str_field_of` copies the field's text) and then hashed the pattern to find
    // its compiled regex — before the `match` had even looked at the method name. The idiom this
    // sits under is `while (m.find()) { ... m.group(1) ... }`, so a k-group scan over an
    // n-character subject copied n characters 1+k times per match. These four arms read a
    // synthetic field and nothing else; served here, they touch neither.
    //
    // One implementation, reached from two places: the `match` below still calls
    // `matcher_field_arm` for the same names, so `RAVA_NO_FAST_MISC=1` restores the old order
    // without a second copy of the bodies.
    if fast_misc() {
        if let Some(v) = matcher_field_arm(vm, recv, name, args) {
            return Some(v);
        }
    }
    let regex = str_field_of(vm, args.first(), "$regex");
    let input = str_field_of(vm, args.first(), "$input");
    let re = match cached_regex(&regex) { Some(r) => r, None => cached_regex(&regex::escape(&regex))? };
    match name {
        "matches" => {
            let full = cached_regex(&format!("^(?:{regex})$"));
            Some(Some(Value::Int(full.map(|r| r.is_match(&input)).unwrap_or(false) as i32)))
        }
        "lookingAt" => Some(Some(Value::Int(re.find(&input).map(|m| m.start() == 0).unwrap_or(false) as i32))),
        "find" => {
            let pos = (vm.heap.get_field_syn(recv, crate::heap::Syn::Pos).as_int().unwrap_or(0) as usize).min(input.len());
            match re.captures_at(&input, pos) {
                Some(caps) => {
                    let m = caps.get(0).unwrap();
                    vm.heap.put_field(recv, "$mstart", Value::Int(m.start() as i32));
                    vm.heap.put_field(recv, "$mend", Value::Int(m.end() as i32));
                    for (i, g) in caps.iter().enumerate() {
                        let v = g.map(|x| Value::Str(x.as_str().into())).unwrap_or(Value::Null);
                        // `group_field` borrows `$g0`..`$g9`; the `format!` here allocated a
                        // field NAME per group per match, and `find` is called in a loop.
                        vm.heap.put_field(recv, &group_field(i as i32), v);
                    }
                    // NAMED GROUPS. `group("user")` took `as_int()` of a String, got
                    // `None`, defaulted to 0 and returned the WHOLE MATCH — a wrong
                    // answer that reads as a right one until a group differs from the
                    // match. Record each name beside its number.
                    for gname in re.capture_names().flatten() {
                        let v = caps
                            .name(gname)
                            .map(|x| Value::Str(x.as_str().into()))
                            .unwrap_or(Value::Null);
                        vm.heap.put_field(recv, &format!("$gn_{gname}"), v);
                    }
                    let next = if m.end() > m.start() { m.end() } else { m.end() + 1 };
                    vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Int(next as i32));
                    Some(Some(Value::Int(1)))
                }
                None => Some(Some(Value::Int(0))),
            }
        }
        "group" => matcher_field_arm(vm, recv, name, args),
        // `appendReplacement(sb, repl)` / `appendTail(sb)` — the JDK's incremental
        // replace loop, and neither existed. `$apos` is the append cursor: everything
        // before the current match and after the previous one is copied verbatim, then
        // the replacement, and `appendTail` copies whatever is left.
        "appendReplacement" | "appendTail" => {
            let sb = args.get(1).cloned().unwrap_or(Value::Null);
            let apos = (vm.heap.get_field(recv, "$apos").as_int().unwrap_or(0) as usize).min(input.len());
            let piece = if name == "appendTail" {
                vm.heap.put_field(recv, "$apos", Value::Int(input.len() as i32));
                input[apos..].to_string()
            } else {
                let ms = (vm.heap.get_field(recv, "$mstart").as_int().unwrap_or(0) as usize).min(input.len());
                let me = (vm.heap.get_field(recv, "$mend").as_int().unwrap_or(0) as usize).min(input.len());
                let rep = str_of(vm, args.get(2)).unwrap_or_default();
                vm.heap.put_field(recv, "$apos", Value::Int(me as i32));
                format!("{}{}", &input[apos..ms], rep)
            };
            // Through the StringBuilder's own append, so either representation works.
            let p = Value::Str(piece.into());
            let _ = crate::natives::strings::sb_dispatch(
                vm,
                "append",
                "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
                &[sb.clone(), p],
            );
            if name == "appendTail" { Some(Some(sb)) } else { Some(Some(Value::Ref(recv))) }
        }
        "start" | "end" | "reset" => matcher_field_arm(vm, recv, name, args),
        "groupCount" => Some(Some(Value::Int(re.captures_len() as i32 - 1))),
        "replaceAll" => {
            let rep = str_of(vm, args.get(1)).unwrap_or_default();
            Some(Some(Value::Str(re.replace_all(&input, rep.as_str()).into_owned().into())))
        }
        "replaceFirst" => {
            let rep = str_of(vm, args.get(1)).unwrap_or_default();
            Some(Some(Value::Str(re.replace(&input, rep.as_str()).into_owned().into())))
        }
        _ => None,
    }
}

/// `Objects.hashCode(o)` = `o == null ? 0 : o.hashCode()`.
///
/// `identity_hash` is the right answer only for a type with no `hashCode` override.
/// For a String it is not: `String.hashCode` is a SPECIFIED value (the 31-fold), the
/// same one rava's own `String.hashCode` native and `record_component_hash` return, so
/// `Objects.hashCode(s)` disagreeing with `s.hashCode()` inside one VM is a plain
/// contradiction. Both String representations resolve here — a `Value::Str` and a heap
/// String object are the same java.lang.String to the guest.
/// Widened to `map::element_hash`, which answers the SAME identity hash for
/// everything whose hash the JVM does not define from the value, and the
/// specified fold for the rest — a String, a boxed Boolean, a `Map$Entry`, a
/// native `$map` Map and an `$elems` List/Set. Those last three are the ones
/// `coll_op`/`map_method` now answer `hashCode()` for, so routing them through a
/// different function here would reintroduce exactly the contradiction this
/// helper exists to prevent. Still no guest frame: every case it adds is computed
/// from heap state.
fn objects_hash_of(vm: &mut Vm, v: Option<&Value>) -> i32 {
    // ⭐**NOTHING IS BUILT HERE TO BE UNWRAPPED IMMEDIATELY.** `element_hash` takes a BORROW, and
    // the old body handed it one by cloning twice — `Option::cloned` to make an owned `Option`,
    // then `unwrap_or` to make an owned `Value` — so every `Objects.hashCode`/`Objects.hash`
    // element paid an `Arc` round-trip (a `Value::Str` owns one) to produce a temporary the
    // callee only reads. Bit-identical: `None` and `Some(Null)` both hashed `Null` before and do
    // now.
    match v {
        Some(v) => crate::map::element_hash(vm, v),
        None => crate::map::element_hash(vm, &Value::Null),
    }
}

/// `IntSummaryStatistics` / `LongSummaryStatistics` / `DoubleSummaryStatistics` — the
/// readers over the five fields `stream_op`'s `summaryStatistics` writes.
///
/// The `toString` shape is the JDK's exactly, including the six-decimal average, because
/// this object exists to be logged. `average` renders with `.` — this engine formats as
/// `Locale.ROOT` throughout, which is the one documented divergence from a comma-decimal
/// JVM (see `bench/StrNum.java`).
pub(crate) fn summary_statistics(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    let Some(Value::Ref(h)) = args.first() else { return Ok(Some(Some(Value::Null))) };
    let h = *h;
    let f = |vm: &mut Vm, n: &str| vm.heap.get_field(h, n);
    let ret = match name {
        "getCount" => f(vm, "$count"),
        "getSum" => f(vm, "$sum"),
        "getMin" => f(vm, "$min"),
        "getMax" => f(vm, "$max"),
        "getAverage" => f(vm, "$average"),
        "toString" => {
            let simple = owner.rsplit('/').next().unwrap_or(owner);
            let count = f(vm, "$count").as_long().unwrap_or(0);
            let avg = f(vm, "$average").as_f64();
            let num = |v: Value| -> String {
                match v {
                    Value::Double(d) => crate::value::java_dtoa(d),
                    other => other.as_long().map(|l| l.to_string()).unwrap_or_else(|| "0".into()),
                }
            };
            let (sum, min, max) = (f(vm, "$sum"), f(vm, "$min"), f(vm, "$max"));
            Value::Str(
                format!(
                    "{simple}{{count={count}, sum={}, min={}, average={avg:.6}, max={}}}",
                    num(sum),
                    num(min),
                    num(max)
                )
                .into(),
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

/// `equals_kind` for a value, without building a class-name `String` on the hot path.
/// `None` = no class (a non-reference value).
///
/// ⛔Measured on `ShapeOr` and worth NOTHING there (13.628/14.534 against 13.707/14.319,
/// overlapping) — the allocation was not the cost either. Kept because it is strictly less work
/// for the same answer, not because it was a win.
///
/// Lifted out of the `equals` arm so `class_equals_kind` below — which is what the arm now calls —
/// can reach it. Body unchanged.
fn eq_kind_of(vm: &mut Vm, g: u8, v: &Value) -> Option<crate::map::EqualsKind> {
    // WAS an acquire `OnceLock` load of its own; IS a bit of the word the caller already holds.
    // See `misc_gates`. The kill switch restores the original static, read here.
    let cheap = mg(g, MG_EQ_CHEAP, || {
        static CHEAP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *CHEAP.get_or_init(|| std::env::var_os("RAVA_NO_EQ_CHEAP_KIND").is_none())
    });
    if cheap {
        let peeked = match v {
            Value::Ref(h) => vm.heap.class_of(*h).and_then(crate::map::equals_kind_peek),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                .and_then(|n| crate::map::equals_kind_peek(&n)),
            _ => None,
        };
        if peeked.is_some() {
            return peeked;
        }
    }
    let class = vm.value_class_name(v);
    if class.is_empty() {
        return None;
    }
    Some(crate::map::equals_kind(vm, &class))
}

/// The two verdicts `Objects.equals` needs about a value's CLASS, cached per class instead of
/// re-derived per call.
///
/// ⭐**TWO MEMO PROBES AND A CLASS-NAME HASH, EVERY CALL, FOR TWO FACTS THAT ARE PROPERTIES OF
/// THE CLASS.** `Objects.equals` is ~350,000 calls a boot and 3.69% of all compiled-to-host
/// crossings (`docs/objects-as-bytecode.md`), and each one asked the same class the same two
/// questions from scratch:
///
///   * "is this class's `equals` reference identity?" — `objects_identity_equals`, already
///     memoised on the class word, so one integer-keyed hashbrown probe;
///   * "what KIND of `equals` does it have?" — `eq_kind_of` → `equals_kind_peek`, memoised on
///     the class NAME, so a word-at-a-time fold of 40-60 characters plus a probe — and, for an
///     ARENA receiver, `nextgen::class_name` first, which clones an `Arc<str>` and drops it
///     again: two atomics for a name used only as a hash key.
///
/// The class word (the interned `Arc<str>` pointer the machine-code PIC already compares) and the
/// arena class id are integers this engine has in hand, and both facts are pure functions of
/// them — so one direct-mapped lookup answers both.
///
/// **Filled ONLY from the two existing memos**, so this is a cache of their answers and never a
/// second source of truth: it cannot disagree with the collection paths that consult the same
/// memos. And the two halves are filled INDEPENDENTLY and lazily, so a call still asks exactly
/// the questions it asked before, in the same order — nothing is derived eagerly. That matters:
/// `equals_kind` resolves a method and reads its bytecode, and making that happen for a class a
/// call would have short-circuited past is a change in class-load order, which is not a change
/// this engine makes as a side effect of a cache.
///
/// An entry can never go stale: classes are never redefined here, which `identity_verdict_word`,
/// `value_equality_by_cid` and `arraymap_ids` all already rely on.
#[derive(Clone, Copy, Default)]
struct ClassEq {
    /// bit 0 = `identity` resolved, bit 1 = `kind` resolved.
    known: u8,
    identity: bool,
    /// 0 = Value, 1 = Identity, 2 = Unknown, 3 = the value has no class at all.
    kind: u8,
}

/// **64 slots × 16 bytes = 1,024 BYTES TOTAL.** Deliberately tiny: this tree measured a
/// direct-mapped table grown from 2.5 KB to 14 KB at **+6.8% WORSE on the boot**, in both orders
/// of an interleaved A/B, because a cache that stops fitting is worse than the work it removes.
/// A boot's `Objects.equals` traffic is a handful of classes, so 64 ways is far more than the
/// live set needs.
const CLASS_EQ_SLOTS: usize = 64;

thread_local! {
    /// Key 0 means "empty" — a class word is an `Arc` pointer and an arena key carries bit 63,
    /// so a real key can never be zero.
    ///
    /// ⭐**THE CHEAP `thread_local` SHAPE, AND IT WAS NOT IN IT.** WAS
    /// `RefCell<[(u64, ClassEq); 64]>` with a RUNTIME initialiser: every `class_eq_get`/`_put`
    /// therefore went through an out-of-line `LocalKey::with` carrying a LAZY-INIT CHECK on the
    /// slot, and then paid `RefCell`'s borrow protocol — a borrow-flag load, a compare, a store,
    /// and a release store when the guard dropped — to read or write 16 bytes of `Copy` data.
    /// IS an array of `Cell`s with a `const` initialiser: the array lives in `.tbss`, so the
    /// access is a TLS-slot address computation with no init check, and `Cell::get`/`set` on a
    /// `Copy` tuple are a plain load and a plain store with no flag at all.
    ///
    /// POPULATION: up to TWO accesses per `Objects.equals` call (`class_identity_equals` and
    /// `class_equals_kind` each probe, and each may write back once per class per thread) at the
    /// ~350,000 calls a boot this arm's census records.
    ///
    /// Nothing about the CONTENTS changed: same 64 slots, same key discipline, same direct-mapped
    /// index, same 1 KB footprint — see the note on `CLASS_EQ_SLOTS` for why the size is load-
    /// bearing. `Cell` is single-threaded by construction and this is thread-local, so the
    /// aliasing rules `RefCell` was enforcing were never being tested here: no reference into the
    /// table outlives the `with` closure, which is what made the borrow flag pure overhead.
    static CLASS_EQ: [std::cell::Cell<(u64, ClassEq)>; CLASS_EQ_SLOTS] = const {
        [const { std::cell::Cell::new((0, ClassEq { known: 0, identity: false, kind: 0 })) };
            CLASS_EQ_SLOTS]
    };
}

/// The cache key for a value's class: the interned class-name pointer for a heap object, or the
/// arena class id with bit 63 set to keep the two key spaces disjoint. `None` means no class.
fn class_eq_key(vm: &Vm, v: &Value) -> Option<u64> {
    match v {
        Value::Ref(h) => vm.heap.object_class_word(*h).map(|w| w as u64).filter(|w| *w != 0),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => Some(crate::nextgen::arena_class_id(*p) as u64 | (1u64 << 63)),
        _ => None,
    }
}

/// Multiply-shift, not a mask: class words are `Arc` allocations whose low bits are near-constant
/// and arena keys are small ids under one set top bit, so masking either would pile the whole
/// live set into a few ways.
#[inline]
fn class_eq_slot(key: u64) -> usize {
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize & (CLASS_EQ_SLOTS - 1)
}

#[inline]
fn class_eq_get(key: u64) -> Option<ClassEq> {
    CLASS_EQ.with(|c| {
        let e = c[class_eq_slot(key)].get();
        (e.0 == key).then_some(e.1)
    })
}

#[inline]
fn class_eq_put(key: u64, e: ClassEq) {
    CLASS_EQ.with(|c| c[class_eq_slot(key)].set((key, e)));
}

/// `objects_identity_equals`, answered from the class cache. Same answer, same source.
fn class_identity_equals(vm: &mut Vm, g: u8, v: &Value) -> bool {
    // WAS `fast_misc()`, i.e. an acquire load of its own, and the twin below made a SECOND one on
    // the same call. IS a bit of the word the arm loaded once. See `misc_gates`.
    let cached = if mg(g, MG_FAST_MISC, fast_misc_legacy) { class_eq_key(&*vm, v) } else { None };
    let Some(key) = cached else {
        return crate::natives::objects_identity_equals(vm, v);
    };
    let mut e = class_eq_get(key).unwrap_or_default();
    if e.known & 1 == 0 {
        e.identity = crate::natives::objects_identity_equals(vm, v);
        e.known |= 1;
        class_eq_put(key, e);
    }
    e.identity
}

/// `eq_kind_of`, answered from the class cache. Same answer, same source.
fn class_equals_kind(vm: &mut Vm, g: u8, v: &Value) -> Option<crate::map::EqualsKind> {
    use crate::map::EqualsKind as K;
    // Same fold as the twin above: one bit of the arm's word, not a second acquire load.
    let cached = if mg(g, MG_FAST_MISC, fast_misc_legacy) { class_eq_key(&*vm, v) } else { None };
    let Some(key) = cached else {
        return eq_kind_of(vm, g, v);
    };
    let mut e = class_eq_get(key).unwrap_or_default();
    if e.known & 2 == 0 {
        e.kind = match eq_kind_of(vm, g, v) {
            Some(K::Value) => 0,
            Some(K::Identity) => 1,
            Some(K::Unknown) => 2,
            None => 3,
        };
        e.known |= 2;
        class_eq_put(key, e);
    }
    match e.kind {
        0 => Some(K::Value),
        1 => Some(K::Identity),
        2 => Some(K::Unknown),
        _ => None,
    }
}

pub(crate) fn objects(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Option<Option<Value>>, VmError> {
    let is_null = |v: Option<&Value>| matches!(v, Some(Value::Null) | None);
    let ret = match name {
        "requireNonNull" => {
            if is_null(args.first()) {
                let msg = str_of(vm, args.get(1)).unwrap_or_default();
                return Err(VmError::Throw(vm.make_exception("java/lang/NullPointerException", &msg)));
            }
            args.first().cloned().unwrap_or(Value::Null)
        }
        "requireNonNullElse" => {
            if is_null(args.first()) { args.get(1).cloned().unwrap_or(Value::Null) }
            else { args.first().cloned().unwrap_or(Value::Null) }
        }
        // requireNonNullElseGet(obj, supplier): obj if non-null, else supplier.get().
        "requireNonNullElseGet" => {
            if is_null(args.first()) {
                let sup = args.get(1).cloned().unwrap_or(Value::Null);
                return Ok(Some(Some(vm.apply(&sup, Vec::new())?.unwrap_or(Value::Null))));
            }
            args.first().cloned().unwrap_or(Value::Null)
        }
        // checkIndex(index, length) — the JDK's own bounds primitive, and the source
        // of the "Index i out of bounds for length n" wording. No arm before, so a
        // guest that used it to validate an index got an UnsatisfiedLinkError.
        "checkIndex" => {
            let i = args.first().and_then(Value::as_int).unwrap_or(0);
            let len = args.get(1).and_then(Value::as_int).unwrap_or(0);
            if i < 0 || i >= len {
                return Err(VmError::Throw(vm.make_exception(
                    "java/lang/IndexOutOfBoundsException",
                    &format!("Index {i} out of bounds for length {len}"),
                )));
            }
            Value::Int(i)
        }
        "isNull" => Value::Int(is_null(args.first()) as i32),
        "nonNull" => Value::Int(!is_null(args.first()) as i32),
        // `Objects.equals(a, b)` is `(a == b) || (a != null && a.equals(b))` — it
        // DISPATCHES to the guest's own equals. Answering identity alone made
        // `Objects.equals(new Point(1,2), new Point(1,2))` false, and Objects.equals
        // is what every generated equals and every JDK-shaped helper delegates to,
        // so a value type's equality silently degraded to reference equality
        // everywhere it was reached through this method.
        "equals" => {
            // ⛔ THE GUEST-DISPATCHING FORM WAS BUILT, MEASURED AND REVERTED (2026-08-14).
            //
            // `Objects.equals(a, b)` really is `(a == b) || (a != null && a.equals(b))`, and
            // answering identity alone really does degrade a value type's equality wherever it is
            // reached through this method — the diagnosis is right and the fix belongs.
            //
            // But dispatching from HERE re-enters the interpreter (`vm.invoke`) from inside a
            // native, on a path compiled code reaches through `rava_dispatch_miss`, and without
            // the native-depth guard every other re-entering native takes. The four gates caught
            // it immediately: ChunkFill and RingScan produced NO hash at all, with
            // `java/lang/InternalError: jit dispatch` from a `StackUnderflow` on an interface call
            // to `TypeFamily.apply(I)` — a datafixers callee nowhere near `Objects.equals`, which
            // is what a corrupted operand-cell contract looks like from the outside.
            //
            // Doing it properly needs the same envelope `invoke_guest_equals`'s other callers
            // have: native_depth_inc/dec around the re-entry and a check that the caller is not
            // holding JIT cells. Reported rather than hacked, per this tree's own rule.
            // ⭐ DECIDE WITHOUT A GUEST FRAME WHERE THE ANSWER IS ALREADY KNOWN.
            //
            // `Objects.equals` is 338,112 calls and **17.3% of all native bridge
            // self-time** in a boot — second only to `Stream#collect` — because
            // every non-identical pair re-entered the interpreter to run the
            // guest's `equals`. Most of those pairs need no interpreter at all:
            // a null against a non-null is false, and two values whose CONTENT
            // key this engine can derive without running guest code (a String,
            // a boxed primitive, a value class whose rendering is already
            // memoised on `$keystr`) are decided by comparing those keys — the
            // SAME derivation `coll_op`'s `contains`/`equals` compare by, so the
            // two cannot disagree.
            //
            // `content_key` falls back to `ekey_mut`, which escalates ONLY for a
            // class whose own `equals` bytecode is substantive; for such a class
            // the key IS its `toString()`, memoised per object. The guest
            // dispatch below therefore remains for exactly the shapes where the
            // answer genuinely depends on running the guest's method.
            // ⭐**FIVE PROCESS CONSTANTS, ONE READ, AT THE ENTRY OF THE ARM THAT USES THEM.**
            // See `misc_gates` for what the five were and why five acquire loads per call is not
            // the same thing as one. Read HERE and not at the top of `objects()`: the sibling arms
            // (`requireNonNull`, `isNull`, `hashCode`, `toString`) consult none of these and must
            // not start paying for a load they never needed.
            let g = misc_gates();
            let quick = match (args.first(), args.get(1)) {
                (Some(Value::Null) | None, Some(Value::Null) | None) => Some(true),
                (Some(Value::Null) | None, _) | (_, Some(Value::Null) | None) => Some(false),
                (Some(a), Some(b)) => match (a, b) {
                    (Value::Int(x), Value::Int(y)) => Some(x == y),
                    (Value::Long(x), Value::Long(y)) => Some(x == y),
                    (Value::Float(x), Value::Float(y)) => Some(x.to_bits() == y.to_bits()),
                    (Value::Double(x), Value::Double(y)) => Some(x.to_bits() == y.to_bits()),
                    (Value::Str(x), Value::Str(y)) => Some(x == y),
                    // ⭐**IDENTITY-EQUALS CLASSES ANSWER HERE.** If the receiver's class
                    // PROVES `equals` is reference identity (`map::keys::equality_is_identity`,
                    // decided from the `equals` BODY in bytecode — `return this == o`, a
                    // delegation to `Object.equals`, or `ACC_ENUM`), then two distinct objects
                    // are unequal and there is nothing to derive. Without this the pair fell to
                    // the content-key machinery below, which is why `bench/SeamCost2.java`
                    // priced `Objects.equals` at **1,620 ns against HotSpot's 2** on a pair of
                    // plain `Object`s. `ref_eq` has already answered the SAME-object case.
                    // `class_identity_equals` IS `objects_identity_equals`, read out of the
                    // per-class verdict cache instead of out of its own memo — see `ClassEq`.
                    (Value::Ref(_) | Value::Obj(_), Value::Ref(_) | Value::Obj(_))
                        if class_identity_equals(vm, g, a) =>
                    {
                        Some(crate::natives::ref_eq(Some(a), Some(b)))
                    }
                    // A cross-KIND pair (a number against a string, a boxed value
                    // against a reference) can never be equal under any `equals`
                    // the JDK defines, and the type-tagged keys say so without a
                    // frame.
                    _ => None,
                },
            };
            if let Some(v) = quick {
                crate::contract_stat::objects_equals(crate::contract_stat::EqOutcome::Quick);
                return Ok(Some(Some(Value::Int(v as i32))));
            }
            // ⭐DECIDE BY THE CONTENT KEY, WHICH IS WHAT THE COMMENT ABOVE PROMISED AND
            // THE CODE NEVER DID. The `quick` match returns `None` for a pair of
            // REFERENCES, so every one of them re-entered the interpreter.
            //
            // MEASURED (`RAVA_CONTRACT_STAT`, one boot): **350,000 calls, of which the
            // cheap tests answered 155 (0.0%) and 349,845 (100.0%) ran a GUEST FRAME.**
            // An earlier fastpath keyed on `EqualsKind::Identity` fired **0 times** and
            // measured exactly zero — the classes reaching here all have a substantive
            // `equals`, which is precisely the case `Identity` cannot help.
            //
            // For those classes `ekey_mut` IS their equality: it keys a class with a
            // substantive `equals` by its `toString()`, memoised per object on `$keystr`
            // (heap) or the arena key memo, and it is the SAME derivation `coll_op`'s
            // `contains`/`equals` compare by — so the two cannot disagree by
            // construction. The first derivation may run the guest's `toString`; every
            // one after it is a lookup, where the re-entry was a frame every time.
            //
            // `Unknown` still re-enters: nothing has been proven about that class.
            //
            // `RAVA_NO_EQUALS_KIND_FASTPATH=1` restores the old route for measurement.
            /// OPT-IN (`RAVA_EQ_PEEK=1`), and the reason it is not default is measured, not
            /// cautious. See `stream::ekey_peek`: restricting the key comparison to keys that
            /// ALREADY EXIST is **-19.1% on `bench/ShapeOr.java`** (11.256/11.024 against
            /// 13.692/13.846, disjoint, bit-exact) and **+2.8% on the product boot**
            /// (51.3/50.2 against 49.3/49.4, disjoint).
            ///
            /// Both directions are real and the reason is the same fact: on the boot a derivation
            /// is an INVESTMENT — the key is memoised on the object and read back by every later
            /// collection op — while ShapeOr builds a fresh pair of coordinate lists per merge, so
            /// it never amortises and each call pays two full `toString()` renders.
            ///
            /// Same runtime class, asked without allocating either name.
            fn same_class(vm: &Vm, a: &Value, b: &Value) -> bool {
                match (a, b) {
                    (Value::Ref(x), Value::Ref(y)) => vm.heap.class_of(*x) == vm.heap.class_of(*y),
                    #[cfg(feature = "next-gen-gc")]
                    (Value::Obj(x), Value::Obj(y)) => {
                        crate::nextgen::arena_class_id(*x) == crate::nextgen::arena_class_id(*y)
                    }
                    _ => false,
                }
            }

            // Both WERE acquire `OnceLock` loads on this exact path — `RAVA_EQ_PEEK` inside the
            // `EqualsKind::Value` match guard, `RAVA_NO_EQUALS_KIND_FASTPATH` as the LEFT operand
            // of the conjunction below, so short-circuiting never saved it. Both are bits of the
            // word now; both restore their own static under the kill switch. `eq_peek_enabled`
            // stays a function called FROM the guard, not a value hoisted above it, so the legacy
            // arm reads it exactly where and as often as it always did.
            fn eq_peek_enabled(g: u8) -> bool {
                mg(g, MG_EQ_PEEK, || {
                    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    *ON.get_or_init(|| std::env::var_os("RAVA_EQ_PEEK").is_some())
                })
            }
            let ek_fast = mg(g, MG_EK_FAST, || {
                static EK_FAST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                *EK_FAST.get_or_init(|| std::env::var_os("RAVA_NO_EQUALS_KIND_FASTPATH").is_none())
            });
            if ek_fast
                && !ref_eq(args.first(), args.get(1))
                && !is_null(args.first())
                && !is_null(args.get(1))
            {
                let a = args.first().cloned().unwrap_or(Value::Null);
                let b = args.get(1).cloned().unwrap_or(Value::Null);
                // ⭐**ASK WITHOUT BUILDING A `String`.** `value_class_name` allocates and then
                // `equals_kind` hashes all of it, on every call — and that, not the key
                // derivation, is what this fast path was actually spending. Peek the memo through
                // the class name BORROWED from the heap (or, for an arena object, from its class
                // id) and only fall back to the allocating route on a cold class, which happens
                // once per class per thread. `RAVA_NO_EQ_CHEAP_KIND=1` restores the old route.
                // ⭐**AND NOT EVEN THE MEMO PROBE, ONCE THE CLASS HAS BEEN SEEN.** Peeking still
                // folds the borrowed name and probes a hashbrown table on EVERY call; the answer
                // is a property of the class, and the class word is an integer already in hand.
                // `class_equals_kind` reads it out of the 1 KB per-class verdict cache and only
                // calls `eq_kind_of` on a cold class — see `ClassEq`.
                let kind = class_equals_kind(vm, g, &a);
                if let Some(kind) = kind {
                    match kind {
                        // Inherited `equals` is reference equality, already false here.
                        crate::map::EqualsKind::Identity => {
                            crate::contract_stat::objects_equals(
                                crate::contract_stat::EqOutcome::Identity,
                            );
                            return Ok(Some(Some(Value::Int(0))));
                        }
                        // Proven content equality: compare the keys the collections use —
                        // ⭐**BUT ONLY IF THEY ARE ALREADY DERIVED.** See `stream::ekey_peek` for
                        // the measurement. Deriving a key here runs the class's `toString()`,
                        // which for a `DoubleList` renders every element through `StringBuilder`,
                        // twice per call, to answer a question the guest's own `equals` settles on
                        // the length. A cache hit is a fast path; a cache miss is not.
                        // `RAVA_EQ_PEEK=1` opts in; the default still derives on miss.
                        crate::map::EqualsKind::Value
                            if !eq_peek_enabled(g)
                                || (crate::stream::ekey_peek(vm, &a).is_some()
                                    && crate::stream::ekey_peek(vm, &b).is_some()) =>
                        {
                            let pa = crate::stream::ekey_peek(vm, &a);
                            let pb = crate::stream::ekey_peek(vm, &b);
                            // Different classes are unequal under any `equals` the JDK
                            // defines, and the keys would not be comparable anyway. Compared
                            // by BORROWED name / class id, so this asks the same question the
                            // allocating `value_class_name` pair asked, without the two `String`s.
                            if !same_class(vm, &a, &b) {
                                crate::contract_stat::objects_equals(
                                    crate::contract_stat::EqOutcome::Identity,
                                );
                                return Ok(Some(Some(Value::Int(0))));
                            }
                            // ⛔**A SIZE BOUND ON THE DERIVED KEY WAS BUILT AND MEASURED ZERO**
                            // (`RAVA_EQ_KEY_CAP`, removed). The cost is not the key's LENGTH — a
                            // cap of 32 bytes changed `ShapeOr` by nothing — it is the derivation
                            // itself, which re-enters the guest's `toString` per call. What the
                            // census says instead is at `stream::ekey_peek`.
                            // ⭐**AND IT IS NEUTRAL ON THE STANDING (COMPILERS-OFF) BENCHMARK**,
                            // which is where this arm also runs and which the shipping A/B did not
                            // cover: ABBA reads on 121.9/112.4 s against off 115.5/117.8,
                            // overlapping. Note the 8.5% spread WITHIN one arm — that is this
                            // box's known wall floor, and it is why the compilers-off boot cannot
                            // resolve an effect this size in either direction.
                            let ka = match pa {
                                Some(x) => x,
                                None => crate::stream::ekey_mut(vm, &a),
                            };
                            let kb = match pb {
                                Some(y) => y,
                                None => crate::stream::ekey_mut(vm, &b),
                            };
                            crate::contract_stat::objects_equals(
                                crate::contract_stat::EqOutcome::Identity,
                            );
                            return Ok(Some(Some(Value::Int((ka == kb) as i32))));
                        }
                        // Value, but at least one key would have to be DERIVED: fall through
                        // to the guest's `equals`, which is both the contract and cheaper.
                        crate::map::EqualsKind::Value => {}
                        crate::map::EqualsKind::Unknown => {}
                    }
                }
            }
            let eq = ref_eq(args.first(), args.get(1))
                || (!is_null(args.first()) && {
                    // THE ENVELOPE THE FIRST ATTEMPT LACKED. `stream::dispatch` and
                    // `map::dispatch` re-enter the interpreter from this same cascade and work,
                    // and the difference is these two lines: while a native holds guest `Value`s
                    // in Rust locals, a collection must not run — `native_depth` is what tells
                    // the collector so. Without them the re-entry surfaced as a `StackUnderflow`
                    // in an unrelated datafixers callback, i.e. as a corrupted operand contract.
                    let a = args.first().cloned().unwrap_or(Value::Null);
                    let b = args.get(1).cloned().unwrap_or(Value::Null);
                    crate::contract_stat::objects_equals(crate::contract_stat::EqOutcome::Guest);
                    crate::frames::native_depth_inc();
                    let r = crate::opcodes::invoke_guest_equals(vm, &a, &b);
                    crate::frames::native_depth_dec();
                    r?
                });
            Value::Int(eq as i32)
        }
        // `Objects.hashCode(o)` is `o == null ? 0 : o.hashCode()` — the guest's own
        // hashCode, not the identity hash, or two equal values hash apart.
        // `record_component_hash` is that dispatch plus the wrapper rules for the
        // unboxed primitive cells. (A boxed `Boolean` is a `Value::Int` in this
        // value model and hashes 1/0 rather than 1231/1237 — indistinguishable here,
        // and no worse than the identity hash it replaces.)
        "hashCode" => {
            // ⛔ SAME REVERT AS `equals` DIRECTLY ABOVE, and for the same measured reason:
            // `record_component_hash` reaches `invoke_guest_hashcode`, which re-enters the
            // interpreter from inside a native. `objects_hash_of` answers the two cases that do
            // not need a guest frame — a String hashes by its SPECIFIED 31-fold, everything else
            // by identity — which is what this arm did before, minus the String contradiction.
            // Same envelope as `equals` above — the guest's own hashCode, with the collector held
            // off while this native holds guest values in Rust locals. `objects_hash_of` remains
            // the answer for the two cases that need no guest frame (a String's SPECIFIED
            // 31-fold, and identity for a type with no override), and is what the dispatch falls
            // back to.
            // ⛔ MEASURED BOUNDARY, not a guess. With the native-depth envelope, the `equals`
            // re-entry directly above passes all four gates; adding the SAME envelope here makes
            // them produce no hash at all again. So the guard is necessary and not sufficient,
            // and whatever `invoke_guest_hashcode` reaches that `invoke_guest_equals` does not is
            // still unowned. `objects_hash_of` answers the two cases that need no guest frame: a
            // String's SPECIFIED 31-fold (so `Objects.hashCode(s)` cannot contradict
            // `s.hashCode()` inside one VM) and identity for everything else.
            // The argument is BORROWED straight out of `args`: the clone existed only to satisfy
            // an owning signature `objects_hash_of` no longer has.
            Value::Int(objects_hash_of(vm, args.first()))
        }
        // `Objects.toString(o)` renders null as "null"; `Objects.toString(o, default)`
        // returns the DEFAULT for a null, and the two-argument form was answering "null"
        // for it — the one thing it exists not to do.
        "toString" => {
            if is_null(args.first()) {
                return Ok(Some(Some(match args.get(1) {
                    Some(d) => d.clone(),
                    None => Value::Str("null".into()),
                })));
            }
            let s = match args.first() { Some(v) => vm.value_to_string(v)?, None => "null".into() };
            Value::Str(s.into())
        }
        // Arrays.hashCode(values): 31-fold over each element's OWN hashCode, same
        // dispatch as `hashCode` above — the identity hash made `Objects.hash(a, b)`
        // differ for equal arguments, which is precisely what a guest hashCode built
        // on it promises not to do.
        "hash" => {
            // As above: the 31-fold is right, the per-element dispatch is not reachable safely
            // from here yet.
            // As `hashCode` above: the fold is right, the per-element guest dispatch is not
            // safely reachable from here yet.
            let vals = varargs(vm, args.first());
            let mut h = 1i32;
            for v in &vals {
                h = h.wrapping_mul(31).wrapping_add(objects_hash_of(vm, Some(v)));
            }
            Value::Int(h)
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
