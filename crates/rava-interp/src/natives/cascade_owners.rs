//! Owner-specific families in the cascade: the classes named one at a time.

use crate::natives::*;

/// Turn a pure native's `(class, message)` into a real in-flight Java throwable.
/// The arithmetic helpers are `&mut Vm`-free by design (they are also called from
/// the arm cache), so they name the exception and the cascade builds it.
fn throwing(vm: &mut Vm, r: Result<Value, NativeThrow>) -> Result<Value, VmError> {
    match r {
        Ok(v) => Ok(v),
        Err((class, msg)) => Err(VmError::Throw(vm.make_exception(class, &msg))),
    }
}

/// `Ok(None)` means this chunk did not serve the call -- the cascade continues.
pub(crate) fn owner_families(
    vm: &mut Vm,
    plan: u32,
    verify: bool,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    use crate::owner_bits as ob;
    let stage = |bit: u32| plan & bit != 0 || verify;
    let _ = stage;
    // ONE memo probe answers every owner question in this function.
    //
    // What it replaces: ~50 exact `owner == "java/..."` compares, an `owner.contains
    // ("ByteBuffer")` that built a fresh `memchr` searcher over a 40-60 byte package path, and
    // two `ends_with` — run in full, per call, for every owner that ends up declining. A PC
    // profile of `bench/CtorBench.java` puts `owner_families` at 3.4% of the constructor path
    // with bare `memchr` at 4.2% beside it, and `memchr` is 3.35% of the compiled profile.
    // The answers are a pure function of the name, so `owner_bits` computes them once per
    // distinct owner into the SAME memo entry the stage plan already came from (no extra
    // memory: the family word lands in the direct slot's existing padding), and every arm
    // below is a bit test on a register.
    //
    // The bits are EXACT, not the superset the stage bits are, because the arms no longer
    // re-test the owner themselves — so `verify` deliberately does NOT widen them the way it
    // widens `stage`. `RAVA_NO_ID_RESOLVE=1` runs each arm's own literals instead, from the
    // same table, so the two arms cannot drift.
    //
    // ⭐**TWO PROCESS CONSTANTS, ONE READ (`RAVA_NO_NAT_GATEWORD=1` restores the two).** This
    // line used to be `ob::enabled() && ob::id_resolve()` — two separate `OnceLock<bool>`s, so
    // two ACQUIRE-ordered atomic loads from two addresses and two branches, on EVERY entry to
    // this function (order 6 M per product boot; the `RAVA_DISPATCH_GATE` census read 7.64 M
    // cascade entries). Both are folded into `nat_gates()`, so the default path is ONE load and
    // a predicted bit test. **PER-ENTRY ACQUIRE LOADS HERE: 2 → 1.**
    //
    // `ob::family_word` below is the gate-free sibling of `plan()`/`bits()` for the OTHER
    // word of the SAME memo entry: it adds ZERO gate reads (it runs only inside the
    // `by_id` arm above, the same `enabled() && id_resolve()` question already answered
    // there), and the memo lookup itself is unchanged.
    let ngates = nat_gates();
    let by_id = if ngates & NG_LEGACY != 0 {
        ob::enabled() && ob::id_resolve()
    } else {
        ngates & NG_FAM_BY_ID != 0
    };
    let word = if by_id { ob::family_word(owner) } else { 0 };
    let fam = |bit: u32| if by_id { word & bit != 0 } else { ob::family_scan(owner, bit) };
    if fam(ob::F_CODER_RESULT) {
        let under = match args.first() {
            Some(Value::Ref(h)) => matches!(vm.heap.get_field(*h, "$underflow"), Value::Int(1)),
            _ => true,
        };
        match name {
            "isUnderflow" => return Ok(Some(Some(Value::Int(under as i32)))),
            "isOverflow" | "isError" | "isMalformed" | "isUnmappable" => return Ok(Some(Some(Value::Int(0)))),
            "throwException" => return Ok(Some(None)),
            "length" => return Ok(Some(Some(Value::Int(0)))),
            _ => {}
        }
    }
    if fam(ob::F_UUID) {
        if let Some(r) = uuid_dispatch(vm, name, descriptor, args)? {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_BASE64) {
        if let Some(r) = base64_dispatch(vm, owner, name, args)? {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_TEXT) {
        if let Some(r) = text_dispatch(vm, owner, name, descriptor, args) {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_ROUNDING_MODE) && matches!(name, "values" | "valueOf") {
        let mk = |vm: &mut Vm, nm: &str, ord: i32| {
            let h = vm.heap.alloc("java/math/RoundingMode");
            vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(nm.into()));
            vm.heap.put_field(h, "$enumName", Value::Str(nm.into()));
            vm.heap.put_field(h, "$enumOrd", Value::Int(ord));
            Value::Ref(h)
        };
        if name == "values" {
            let vals: Vec<Value> = ROUNDING_MODES.iter().enumerate().map(|(i, n)| mk(vm, n, i as i32)).collect();
            return Ok(Some(Some(Value::Array(vm.heap.alloc_values("Ljava/math/RoundingMode;", vals)))));
        }
        let nm = str_of(vm, args.first()).unwrap_or_default();
        let ord = ROUNDING_MODES.iter().position(|m| *m == nm).unwrap_or(0) as i32;
        return Ok(Some(Some(mk(vm, &nm, ord))));
    }
    // Generic Enum methods for synthetic JDK enum constants (RoundingMode,
    // TimeUnit, ChronoUnit, ...) whose class isn't in the jar, so Enum's methods
    // don't resolve through the super-chain. They carry $enumName/$enumOrd.
    // `enum_field` reads the two synthetic cells from either a handle (Value::Ref)
    // or an arena-born enum (Value::Obj, RAVA_ARENA_ENUM) — flag OFF keeps the
    // Ref-only historic behaviour.
    if matches!(name, "ordinal" | "name" | "compareTo") {
        let recv = args.first();
        let is_enum = matches!(enum_field(vm, recv, "$enumName"), Some(Value::Str(_)));
        if is_enum {
            match name {
                "ordinal" => return Ok(Some(Some(Value::Int(
                    enum_field(vm, recv, "$enumOrd").and_then(|v| v.as_int()).unwrap_or(0))))),
                "name" => return Ok(Some(Some(
                    enum_field(vm, recv, "$enumName").unwrap_or(Value::Null)))),
                "compareTo" => {
                    // RAW difference, not its sign: `Enum.compareTo` is specified as
                    // `self.ordinal - other.ordinal`, and guest code prints it or
                    // arithmetically combines it (a comparator chain that scales a
                    // sub-comparison by the previous result). `signum` collapsed
                    // RED.compareTo(BLUE) from -2 to -1.
                    let a = enum_field(vm, recv, "$enumOrd").and_then(|v| v.as_int()).unwrap_or(0);
                    let b = enum_field(vm, args.get(1), "$enumOrd").and_then(|v| v.as_int()).unwrap_or(0);
                    return Ok(Some(Some(Value::Int(a - b))));
                }
                _ => {}
            }
        }
    }
    if fam(ob::F_BITSET) {
        if let Some(r) = bitset_dispatch(vm, name, descriptor, args) {
            return Ok(Some(r));
        }
    }
    // gson's LazilyParsedNumber holds its number text in a `value` String field
    // and lazily converts. Its Number accessors (+ the *Exact variants) aren't
    // overridden as bytecode we resolve, so service them from the text.
    if fam(ob::F_LAZY_NUMBER) {
        // ⭐**ONE STRING COPY REMOVED PER ACCESSOR.** `get_field_hint` already hands back an
        // owned `Value::Str`, whose `RStr` is an `Arc<String>` the field read has ALREADY
        // cloned; `.to_string()` on top of it allocated a SECOND copy of the same digits only to
        // `trim()` and `parse()` a borrowed `&str` out of it. Hold the `Value` and borrow through
        // it instead. Population: every gson `LazilyParsedNumber` number accessor — the datapack
        // JSON parse path, which the phase map puts at 44% of a product boot's wall.
        let sv = match args.first() {
            Some(Value::Ref(h)) => {
                let id = vm.resolve_field_id("com/google/gson/internal/LazilyParsedNumber", "value");
                vm.heap.get_field_hint(*h, id, 0).0
            }
            _ => Value::Null,
        };
        let t = match &sv {
            Value::Str(s) => s.as_str().trim(),
            _ => "",
        };
        let asi64 = || t.parse::<i64>().or_else(|_| t.parse::<f64>().map(|f| f as i64)).unwrap_or(0);
        let r = match name {
            "longValue" | "longValueExact" => Value::Long(asi64()),
            "intValue" | "intValueExact" => Value::Int(asi64() as i32),
            "shortValue" | "shortValueExact" | "byteValue" | "byteValueExact" => Value::Int(asi64() as i32),
            "doubleValue" => Value::Double(t.parse().unwrap_or(0.0)),
            "floatValue" => Value::Float(t.parse().unwrap_or(0.0)),
            "toString" => Value::Str(t.into()),
            "hashCode" => Value::Int(t.bytes().fold(0i32, |a, b| a.wrapping_mul(31).wrapping_add(b as i32))),
            _ => return Ok(None),
        };
        return Ok(Some(Some(r)));
    }
    // java.math.BigDecimal / BigInteger: gson parses JSON datapack numbers into
    // these (NumberLimits.parseBigDecimal → `new BigDecimal(s).scale()`), and MC's
    // codecs then read the value. No JDK bytecode for them, so back every method by
    // the canonical decimal text stored at construction (honest field read: the
    // String value lives on the object; scale/precision/intValue/… derive from it).
    if fam(ob::F_BIGNUM) {
        if let Some(r) = bignum_dispatch(vm, owner, name, args)? {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_MESSAGE_DIGEST) {
        if let Some(r) = messagedigest_dispatch(vm, name, descriptor, args) {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_KEY) {
        if let Some(r) = keypair_dispatch(vm, owner, name, args) {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_BYTE_BUFFER) {
        if let Some(r) = bytebuffer_dispatch(vm, name, args)? {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_CHAR_BUFFER) {
        if let Some(r) = charbuffer_dispatch(vm, name, args) {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_TYPED_BUFFER) {
        if let Some(r) = typed_buffer_dispatch(vm, owner, name, args) {
            return Ok(Some(r));
        }
    }
    if fam(ob::F_REGEX) {
        if let Some(r) = regex_dispatch(vm, owner, name, args)? {
            return Ok(Some(r));
        }
    }
    if stage(ob::S_TIME) {
        if let Some(r) = time_dispatch(vm, owner, name, args) {
            if plan & ob::S_TIME == 0 {
                gate_stage_miss("time", owner, name, descriptor);
            }
            return Ok(Some(r));
        }
    }
    if fam(ob::F_MATH) {
        // Math.random(): a process-wide pseudo-random double in [0,1). The JDK
        // backs it with a shared Random; a single advancing xorshift seed is
        // equivalent for callers that just want noise (MC BlockFluids fluid
        // spread, particle/sound jitter). Single-threaded, so the non-atomic
        // read-modify-write of the static seed is fine.
        if name == "random" {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEED: AtomicU64 = AtomicU64::new(0x9E3779B9_7F4A7C15);
            let mut s = SEED.load(Ordering::Relaxed);
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            SEED.store(s, Ordering::Relaxed);
            let d = (s >> 11) as f64 / (1u64 << 53) as f64;
            return Ok(Some(Some(Value::Double(d))));
        }
        if let Some(r) = math_dispatch(name, descriptor, args) {
            return Ok(Some(Some(throwing(vm, r)?)));
        }
    }
    if fam(ob::F_CHARACTER) {
        // codePointAt/codePointCount take a CharSequence + index, not a char.
        match name {
            "codePointAt" | "codePointBefore" => {
                let s = str_of(vm, args.first()).unwrap_or_default();
                let chars: Vec<char> = s.chars().collect();
                let i = args.get(1).and_then(Value::as_int).unwrap_or(0);
                let idx = if name == "codePointBefore" { i - 1 } else { i };
                let cp = chars.get(idx.max(0) as usize).map(|c| *c as i32).unwrap_or(0);
                return Ok(Some(Some(Value::Int(cp))));
            }
            "codePointCount" => {
                let s = str_of(vm, args.first()).unwrap_or_default();
                let a = args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
                let b = args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
                let chars: Vec<char> = s.chars().collect();
                let n = b.min(chars.len()).saturating_sub(a.min(chars.len()));
                return Ok(Some(Some(Value::Int(n as i32))));
            }
            _ => {}
        }
        if let Some(r) = char_dispatch(vm, name, args) {
            return Ok(Some(Some(r)));
        }
    }
    if fam(ob::F_INT_LONG) {
        if let Some(r) = int_static(fam(ob::F_LONG), name, args) {
            note_arm_for(owner, name, ARM_INT);
            return Ok(Some(Some(throwing(vm, r)?)));
        }
    }
    if fam(ob::F_STRING) {
        if let Some(r) = str_dispatch(vm, name, descriptor, args)? {
            note_arm_for(owner, name, ARM_STR);
            return Ok(Some(Some(r)));
        }
    }
    if fam(ob::F_WRITER) {
        if let Some(r) = writer_dispatch(vm, owner, name, descriptor, args)? {
            note_arm_for(owner, name, ARM_WRITER);
            return Ok(Some(r));
        }
    }
    if fam(ob::F_STRING_BUILDER) {
        if let Some(r) = sb_dispatch(vm, name, descriptor, args)? {
            note_arm_for(owner, name, ARM_SB);
            return Ok(Some(r));
        }
    }
    // A captured stack frame. Served here rather than in the throwable arm because its
    // owner is neither `Throwable` nor an `*Exception`/`*Error` name, and `getStackTrace`
    // hands these objects to the guest, which then calls the accessors on them.
    if fam(ob::F_STACK_TRACE_ELEMENT) {
        // BORROW, DON'T CLONE: `args.first().cloned()` duplicated the whole `Value` — a call to
        // `Value::clone`, which is a tag test and a 24-byte copy, and an ATOMIC increment plus a
        // matching decrement when the receiver happens to be a `Value::Str` — to read a u32
        // handle out of it and drop the rest. The pattern copies the `u32` and touches nothing.
        if let Some(&Value::Ref(h)) = args.first() {
            let r = match (name, descriptor) {
                ("getClassName", "()Ljava/lang/String;") => Some(vm.heap.get_field(h, "$declaringClass")),
                ("getMethodName", "()Ljava/lang/String;") => Some(vm.heap.get_field(h, "$methodName")),
                // No LineNumberTable is parsed anywhere in the tree, so the honest answers
                // are the JVM's own "unknown" ones rather than invented numbers.
                ("getFileName", "()Ljava/lang/String;") => Some(Value::Null),
                ("getLineNumber", "()I") => Some(Value::Int(-1)),
                ("isNativeMethod", "()Z") => Some(Value::Int(0)),
                ("toString", "()Ljava/lang/String;") => {
                    Some(Value::Str(crate::stacktrace::element_to_string(vm, h).into()))
                }
                _ => None,
            };
            if let Some(v) = r {
                return Ok(Some(Some(v)));
            }
        }
    }
    // Throwable hierarchy (JDK supers of jar exception subclasses).
    if fam(ob::F_THROWABLE) {
        if let Some(r) = throwable(vm, owner, name, descriptor, args)? {
            return Ok(Some(r));
        }
    }
    // GZIP/Inflater input: eagerly decompress the wrapped stream into our $buf so
    // reads (and a wrapping DataInputStream) serve inflated NBT. Drain the source
    // through its read(byte[]) contract; unmodeled guest wrappers use their resolved
    // bytecode body rather than requiring class-specific buffer knowledge.
    if fam(ob::F_ZIP_INPUT) && name == "<init>" {
        // Same borrow-instead-of-clone as the stack-trace arm above: one `Value::clone` (and its
        // atomic pair on a `Str` receiver) removed per `<init>` reaching this arm.
        if let Some(&Value::Ref(r)) = args.first() {
            // Drain the wrapped source through its OWN read path (stream_read_all now
            // follows the wrapper chain AND serves any PushbackInputStream unread bytes
            // first — NbtIo.readCompressed peeks+unreads the gzip magic before handing
            // the stream to GZIPInputStream, so that magic must lead the inflate input).
            let raw = match stream_read_all(vm, args.get(1))? {
                Value::Array(a) => vm.heap.array_bytes(a).unwrap_or_default(),
                _ => Vec::new(),
            };
            let dec = crate::nio::gunzip_bytes(&raw);
            let buf = vm.heap.alloc_bytes(&dec);
            vm.heap.put_field(r, "$buf", Value::Array(buf));
            vm.heap.put_field_syn(r, crate::heap::Syn::Pos, Value::Int(0));
        }
        return Ok(Some(None));
    }
    Ok(None)
}
