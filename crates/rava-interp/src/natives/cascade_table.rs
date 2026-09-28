//! The cascade tail, first half: one match over (owner, name, descriptor).

use crate::natives::*;

/// `Ok(None)` = no arm matched; the cascade continues.
pub(crate) fn owner_table(
    vm: &mut Vm,
    plan: u32,
    verify: bool,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    let stage = |bit: u32| plan & bit != 0 || verify;
    let _ = (stage, plan, verify);
    let handled: Option<Value> = match (owner, name, descriptor) {
        // netty's Unix/epoll errno table (JNI) — never used on the NIO transport
        // we run, but its <clinit> reads these. Return the Linux errno values.
        ("io/netty/channel/unix/ErrorsStaticallyReferencedJniMethods", n, _) => Some(match n {
            "errnoEAGAIN" | "errnoEWOULDBLOCK" => Value::Int(11),
            "errnoEBADF" => Value::Int(9),
            "errnoEPIPE" => Value::Int(32),
            "errnoENOENT" => Value::Int(2),
            "errnoECONNRESET" => Value::Int(104),
            "errnoENOTCONN" => Value::Int(107),
            "errnoEINPROGRESS" => Value::Int(115),
            "errorEALREADY" => Value::Int(114),
            "errorECONNREFUSED" => Value::Int(111),
            "errorENETUNREACH" => Value::Int(101),
            "errorEISCONN" => Value::Int(106),
            "strError" => Value::Str("error".into()),
            _ => Value::Int(0),
        }),
        // java.util.Timer — we have no timer thread; scheduling is a no-op for
        // boot (watchdog/metrics tasks just don't fire), cancel/purge benign.
        ("java/util/Timer", "schedule" | "scheduleAtFixedRate" | "cancel", _) => None,
        ("java/util/Timer", "purge", _) => Some(Value::Int(0)),
        // No display: the engine is always headless (the server runs nogui).
        // MinecraftServer checks this before touching any AWT/Swing GUI path.
        ("java/awt/GraphicsEnvironment", "isHeadless", _) => Some(Value::Int(1)),
        // java.lang.System properties.
        ("java/lang/System", "getProperty", "(Ljava/lang/String;)Ljava/lang/String;") => {
            // Alloc-histogram sentinels (RAVA_ALLOC_HISTO): a bench brackets a phase by reading
            // "rava.histo.reset" before it and "rava.histo.dump" after. No-op unless gated on;
            // the sentinel keys have no real property, so we still fall through to a Null return.
            if crate::allochisto::on() {
                match str_of(vm, args.first()).as_deref() {
                    Some("rava.histo.reset") => crate::allochisto::reset(),
                    Some("rava.histo.dump") => crate::allochisto::dump(),
                    _ => {}
                }
            }
            // 14.1: phase-end trigger for the boot-dispatch inspector (e.g. right after
            // Bootstrap/datapack load) so the TOP-20 reflects SETUP, not the whole run.
            if crate::bootdisp::on() && matches!(str_of(vm, args.first()).as_deref(), Some("rava.dispatch.dump")) {
                crate::bootdisp::dump();
            }
            // Wall-profile / bridge-stat phase brackets. Both profilers are cumulative and
            // dump on a fixed timer or at exit, which fuses a phase under study with the
            // 55s bootstrap that precedes it. A bench reads "rava.wall.reset" before the
            // phase and "rava.wall.dump" after (same for "rava.bridge.*") to get that
            // phase's own distribution. Each is a no-op unless its own gate is on, and the
            // sentinel keys have no real property so the return value is unchanged (Null).
            // The dump keys take a free-form suffix ("rava.wall.dump.PARSE") which is echoed
            // as the dump tag, so several bracketed phases stay distinguishable in one log.
            if crate::rava_sampler::enabled() || bridgestat::enabled() {
                if let Some(k) = str_of(vm, args.first()).as_deref() {
                    if k == "rava.wall.reset" {
                        crate::rava_sampler::reset();
                    } else if let Some(tag) = k.strip_prefix("rava.wall.dump") {
                        crate::rava_sampler::dump_tagged(tag.trim_start_matches('.'));
                    } else if k == "rava.bridge.reset" {
                        bridgestat::reset();
                    } else if let Some(tag) = k.strip_prefix("rava.bridge.dump") {
                        bridgestat::dump_tagged(tag.trim_start_matches('.'));
                    }
                }
            }
            match property(args.first()) {
                Some(v) => Some(Value::Str(v.into())),
                None => Some(Value::Null),
            }
        }
        ("java/lang/System", "getProperty", "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;") => {
            match property(args.first()) {
                Some(v) => Some(Value::Str(v.into())),
                None => Some(args.get(1).cloned().unwrap_or(Value::Null)),
            }
        }
        ("java/lang/System", "lineSeparator", "()Ljava/lang/String;") => Some(Value::Str("\n".into())),
        ("java/lang/System", "getSecurityManager", _) => Some(Value::Null),
        ("java/lang/System", "mapLibraryName", _) => {
            let n = str_of(vm, args.first()).unwrap_or_default();
            let mapped = if cfg!(windows) { format!("{n}.dll") } else if cfg!(target_os = "macos") { format!("lib{n}.dylib") } else { format!("lib{n}.so") };
            Some(Value::Str(mapped.into()))
        }
        // No real native libraries — signal failure so callers disable the feature.
        ("java/lang/System", "loadLibrary", _) | ("java/lang/System", "load", _) => {
            let n = args.first().map(Value::display).unwrap_or_default();
            return Err(VmError::Throw(vm.make_exception("java/lang/UnsatisfiedLinkError", &format!("no {n} in rava"))));
        }
        ("java/lang/System", "getProperties", _) => Some(Value::Ref(vm.heap.alloc("java/util/Properties"))),
        ("java/lang/System", "setProperty", _) | ("java/lang/System", "clearProperty", _) => Some(Value::Null),
        ("java/lang/System", "exit", _) | ("java/lang/Runtime", "halt", _) => {
            let code = args.last().and_then(Value::as_int).unwrap_or(0);
            // ⚠**THE END-OF-RUN CENSUSES NEVER PRINTED FOR A REAL SERVER.** `run.rs` dumps them
            // after the guest `main` RETURNS, and a Minecraft server ends by calling
            // `System.exit` from its shutdown thread — so a boot driven to `stop` produced no
            // `[JIT-CB]`, no `[BRIDGE]`, no `[CHAIN]`, only silence that read like "the census is
            // off". Same failure mode already recorded for the five tables that printed only
            // from `lane_probe`, one layer further out: the dump has to be on the path the
            // PRODUCT actually exits by, not the one the tests exit by.
            crate::diag::dump("System.exit");
            crate::natives::bridgestat::dump();
            crate::chain_census_report();
            crate::seam_census_report();
            crate::natives::cascade::fast_cascade_report();
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(code);
        }
        // Stream redirection — we keep using the real process stdout/stderr.
        ("java/lang/System", "setOut" | "setErr" | "setIn" | "setSecurityManager", _) => None,
        ("java/lang/System", "getenv", "(Ljava/lang/String;)Ljava/lang/String;") => {
            let v = str_of(vm, args.first()).and_then(|s| std::env::var(&s).ok());
            Some(v.map(|s| Value::Str(s.into())).unwrap_or(Value::Null))
        }
        ("java/lang/System", "getenv", "()Ljava/util/Map;") => {
            let h = vm.heap.alloc("java/util/HashMap");
            let id = vm.heap.alloc_map();
            vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
            for (k, val) in std::env::vars() {
                vm.heap.map_put(id, crate::heap::MapKey::str(format!("s:{k}")), Value::Str(k.into()), Value::Str(val.into()));
            }
            Some(Value::Ref(h))
        }
        ("java/lang/System", "currentTimeMillis", "()J") => Some(Value::Long(epoch_millis())),
        ("java/lang/System", "nanoTime", "()J") => Some(Value::Long(epoch_millis().wrapping_mul(1_000_000))),
        ("java/lang/System", "identityHashCode", _) => Some(Value::Int(identity_hash(args.first()))),
        ("java/lang/System", "arraycopy", _) => {
            if let (Some(Value::Array(src)), Some(Value::Array(dst))) = (args.first(), args.get(2)) {
                let (src, dst) = (*src, *dst);
                let sp = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize;
                let dp = args.get(3).and_then(Value::as_int).unwrap_or(0) as usize;
                let len = args.get(4).and_then(Value::as_int).unwrap_or(0) as usize;
                crate::opcodes::arrays::arraycopy_census::bump_attempt(len);
                if std::env::var_os("RAVA_NO_ARRAYCOPY_FAST").is_none()
                    && crate::opcodes::arrays::bulk_arraycopy(&mut vm.heap, src, sp, dst, dp, len) {
                    return Ok(Some(None));
                }
                crate::opcodes::arrays::arraycopy_census::bump_fallback();
                // Cold fallback — heterogeneous kinds / biome census / unknown handle.
                // JLS: overlapping same-array copies behave as if via a temp buffer.
                // A forward loop with src==dst && dp>sp reads already-overwritten
                // cells (fastutil add(0,v) shifted zeros over the tail — the
                // NoiseParameters amplitude corruption). Copy backwards there.
                if src == dst && dp > sp {
                    for i in (0..len).rev() {
                        if let Some(v) = vm.heap.array_get(src, sp + i) {
                            vm.heap.array_set(dst, dp + i, v);
                        }
                    }
                } else {
                    for i in 0..len {
                        if let Some(v) = vm.heap.array_get(src, sp + i) {
                            vm.heap.array_set(dst, dp + i, v);
                        }
                    }
                }
            }
            None
        }

        // Boxing — Rava keeps primitives unboxed (Value::Int/Long/...), so
        // Static compare(prim, prim) → -1/0/1.
        ("java/lang/Double", "compare", "(DD)I") | ("java/lang/Float", "compare", "(FF)I") => {
            let to_f = |v: Option<&Value>| -> f64 {
                match v { Some(Value::Double(d)) => *d, Some(Value::Float(f)) => *f as f64,
                    Some(x) => x.as_int().unwrap_or(0) as f64, None => 0.0 }
            };
            let (a, b) = (to_f(args.first()), to_f(args.get(1)));
            // `partial_cmp` reported Equal for (0.0, -0.0) and — via unwrap_or(0)
            // — Equal for any pair involving NaN, so a TreeMap keyed on doubles
            // merged distinct keys and a NaN sorted wherever it happened to land.
            Some(Value::Int(java_dcmp(a, b)))
        }
        ("java/lang/Long", "compare", "(JJ)I") => {
            let a = args.first().and_then(Value::as_long).unwrap_or(0);
            let b = args.get(1).and_then(Value::as_long).unwrap_or(0);
            Some(Value::Int((a > b) as i32 - (a < b) as i32))
        }
        ("java/lang/Integer", "compare", "(II)I")
        | ("java/lang/Short", "compare", "(SS)I") | ("java/lang/Byte", "compare", "(BB)I")
        | ("java/lang/Character", "compare", "(CC)I") | ("java/lang/Boolean", "compare", "(ZZ)I") => {
            // Primitive descriptors, so a box should never arrive -- `int_of` is the belt: it is
            // `as_int` for every input these signatures admit, and differs only for a `Ref`,
            // which `as_int` would have read as 0.
            let a = int_of_or0(vm, args.first());
            let b = int_of_or0(vm, args.get(1));
            Some(Value::Int((a > b) as i32 - (a < b) as i32))
        }
        ("java/lang/Integer" | "java/lang/Long", "compareUnsigned", _) => {
            let a = args.first().and_then(Value::as_long).unwrap_or(0) as u64;
            let b = args.get(1).and_then(Value::as_long).unwrap_or(0) as u64;
            Some(Value::Int((a > b) as i32 - (a < b) as i32))
        }
        // IEEE-754 bit reinterpretation between floating-point and integer.
        ("java/lang/Double", "longBitsToDouble", _) => {
            Some(Value::Double(f64::from_bits(args.first().and_then(Value::as_long).unwrap_or(0) as u64)))
        }
        ("java/lang/Double", "doubleToLongBits", _) | ("java/lang/Double", "doubleToRawLongBits", _) => {
            let d = match args.first() { Some(Value::Double(d)) => *d, Some(v) => v.as_int().unwrap_or(0) as f64, None => 0.0 };
            Some(Value::Long(d.to_bits() as i64))
        }
        ("java/lang/Float", "intBitsToFloat", _) => {
            Some(Value::Float(f32::from_bits(args.first().and_then(Value::as_int).unwrap_or(0) as u32)))
        }
        ("java/lang/Float", "floatToIntBits", _) | ("java/lang/Float", "floatToRawIntBits", _) => {
            let f = match args.first() { Some(Value::Float(f)) => *f, Some(v) => v.as_int().unwrap_or(0) as f32, None => 0.0 };
            Some(Value::Int(f.to_bits() as i32))
        }
        // str_of (not a bare Value::Str match): the argument may be a heap String
        // object, not a Value::Str — gson hands LazilyParsedNumber the number text
        // as a heap String, so parseFloat/parseDouble of it returned 0.
        // valueOf(String) is listed too: the identity `valueOf` arm further down
        // would otherwise hand the STRING back as the boxed number.
        ("java/lang/Double" | "java/lang/Float", "parseDouble" | "parseFloat", _)
        | ("java/lang/Double", "valueOf", "(Ljava/lang/String;)Ljava/lang/Double;")
        | ("java/lang/Float", "valueOf", "(Ljava/lang/String;)Ljava/lang/Float;") => {
            let s = str_of(vm, args.first()).unwrap_or_default();
            match parse_java_fp(&s, owner == "java/lang/Float") {
                Ok(v) => Some(v),
                Err((class, msg)) => return Err(VmError::Throw(vm.make_exception(class, &msg))),
            }
        }
        ("java/lang/Integer" | "java/lang/Long", "decode", _) => {
            let s = str_of(vm, args.first()).unwrap_or_default();
            match decode_java_int(&s, owner == "java/lang/Long") {
                Ok(v) => Some(v),
                Err((class, msg)) => return Err(VmError::Throw(vm.make_exception(class, &msg))),
            }
        }
        ("java/lang/Double" | "java/lang/Float", "isNaN", _) => {
            Some(Value::Int(args.first().map(|v| v.as_f64().is_nan()).unwrap_or(false) as i32))
        }
        ("java/lang/Double" | "java/lang/Float", "isInfinite", _) => {
            Some(Value::Int(args.first().map(|v| v.as_f64().is_infinite()).unwrap_or(false) as i32))
        }
        ("java/lang/Double" | "java/lang/Float", "isFinite", _) => {
            Some(Value::Int(args.first().map(|v| v.as_f64().is_finite()).unwrap_or(false) as i32))
        }
        // Number.xxxValue() cross-conversions on a boxed primitive (e.g.
        // Integer.longValue() -> J, used by MinMaxBounds during advancement load).
        ("java/lang/Integer" | "java/lang/Long" | "java/lang/Short" | "java/lang/Byte"
        | "java/lang/Float" | "java/lang/Double" | "java/lang/Character" | "java/lang/Number",
            m @ ("longValue" | "intValue" | "shortValue" | "byteValue" | "doubleValue" | "floatValue"), _) =>
        {
            let f = match args.first() {
                Some(Value::Double(d)) => *d,
                Some(Value::Float(x)) => *x as f64,
                // `long_of`, not `as_long`: a boxed `Character`/`Short`/`Byte` is a real object
                // now (see the `valueOf` arms below and `crate::boxid`), and `as_long` answers
                // `None` for a `Ref` — so `Short.valueOf((short)9).intValue()` would have read
                // 0. Byte-identical for every other input, which is the whole population before
                // this work.
                Some(v) => long_of(vm, Some(v)) as f64,
                None => 0.0,
            };
            Some(match m {
                "longValue" => Value::Long(f as i64),
                "doubleValue" => Value::Double(f),
                "floatValue" => Value::Float(f as f32),
                _ => Value::Int(f as i32), // intValue / shortValue / byteValue
            })
        }
        // ⭐**THE THREE NARROW BOXES CARRY THEIR OWN TYPE; THE FOUR WIDE ONES DO NOT NEED TO.**
        //
        // `valueOf` used to be one identity arm over all seven, and for `Integer`/`Long`/
        // `Float`/`Double` it still is: the `Value` variant already NAMES the type and renders
        // exactly as Java renders the box, so nothing is erased. For `Character`, `Short` and
        // `Byte` there is no variant, they all arrived as `Value::Int`, and the erasure was
        // observable — `getClass()` answered `java.lang.Integer`, and every path that rendered
        // the OBJECT printed the code point:
        //
        //   java:  java.lang.Character |#|  map {#=1, x=2}   {"key":{"#":"minecraft:paper"}}
        //   rava:  java.lang.Integer   |35| map {35=1, …}    {"key":{"35":"minecraft:paper"}}
        //
        // The last column is `Recipe.CODEC` over a shaped recipe's `Map<Character, Ingredient>`
        // (bench/CompSem.java `recipe.encode`, and the `HASH` of bench/ReloadBench.java with
        // it). So those three BOX, into a real heap object whose `$value` holds the payload —
        // the same fix, the same shape and the same `$value` field `java/lang/Boolean` below
        // already uses, with the JDK's own interning ranges. `crate::boxid` carries the
        // mechanism, the census and the kill switch; `RAVA_NO_BOXID=1` restores this arm to the
        // seven-owner identity it was, byte for byte.
        //
        // `charValue`/`shortValue`/`byteValue` are the matching UNBOX and must read the payload:
        // `Value::as_int` answers `None` for a `Ref`, so a receiver that is now a box would have
        // read zero. (`shortValue`/`byteValue` are also in the `Number` arm above, which the
        // same helper now covers; `charValue` is not in that arm's method list and only ever had
        // this one.)
        ("java/lang/Character" | "java/lang/Short" | "java/lang/Byte", "valueOf", d)
            if crate::boxid::enabled() =>
        {
            let kind = crate::boxid::Kind::of_class(owner).expect("arm head names the three");
            // `Short.valueOf(String)` / `Byte.valueOf(String)` PARSE — the identity arm handed
            // the STRING back as the boxed number, the same mistake `Double.valueOf(String)`
            // above is listed to avoid. `Character` has no such overload.
            if d.starts_with("(Ljava/lang/String;") {
                let s = str_of(vm, args.first()).unwrap_or_default();
                let radix = args.get(1).and_then(Value::as_int).unwrap_or(10);
                let n = match parse_java_int(&s, radix, false, false) {
                    Ok(v) => v.as_int().unwrap_or(0),
                    Err((class, msg)) => return Err(VmError::Throw(vm.make_exception(class, &msg))),
                };
                // The JDK range-checks against the NARROW type, not against `int`:
                // `Short.valueOf("40000")` is a NumberFormatException, not a wrap to -25536.
                if kind.narrow(n) != n {
                    // Named from the OWNER, not from a two-way test on the kind: `Character` has
                    // no `valueOf(String)` overload, so it cannot reach here — and a message
                    // that would call it "Byte" if it ever did is the kind of wrong text a
                    // future overload would inherit silently.
                    let what = owner.rsplit('/').next().unwrap_or(owner);
                    return Err(VmError::Throw(vm.make_exception(
                        "java/lang/NumberFormatException",
                        &format!("Value out of range. Value:\"{s}\" Radix:{radix} for {what}"),
                    )));
                }
                Some(vm.narrow_box(kind, n))
            } else {
                let n = int_of_or0(vm, args.first());
                Some(vm.narrow_box(kind, n))
            }
        }
        // `shortValue`/`byteValue` are NOT here: the `Number` cross-conversion arm above owns
        // them for these owners (it lists both), and it now unboxes through `long_of`. Only
        // `charValue` is absent from that arm's method list, so only `charValue` needs its own.
        ("java/lang/Character", "charValue", _) => Some(Value::Int(char_of(vm, args.first()))),
        // valueOf is an identity for the four boxes the `Value` model already names, and — with
        // `RAVA_NO_BOXID=1` — for all seven, exactly as it was.
        ("java/lang/Integer", "valueOf", _)
        | ("java/lang/Long", "valueOf", _)
        | ("java/lang/Double", "valueOf", _)
        | ("java/lang/Float", "valueOf", _)
        | ("java/lang/Short", "valueOf", _) | ("java/lang/Byte", "valueOf", _)
        | ("java/lang/Character", "valueOf", _) | ("java/lang/Character", "charValue", _) => {
            Some(args.first().cloned().unwrap_or(Value::Null))
        }
        // Enum.valueOf(Class<E>, String): the JDK base method every generated enum
        // `valueOf(String)` delegates to. The named constant is a public static
        // final field of the enum class, so resolve the class from the Class arg
        // and read that static (ensure_init runs <clinit> to populate it). Backs
        // CraftSound and any enum lookup-by-name during play.
        ("java/lang/Enum", "valueOf", _) => {
            let cls = class_field_name(vm, args.first());
            // A name that names no constant is an ERROR, not a null: the JDK's
            // Enum.valueOf ends `throw new IllegalArgumentException("No enum constant "
            // + enumType.getCanonicalName() + "." + name)`. Resolving to Null instead
            // meant `Color.valueOf("purple")` handed the caller a null enum that then
            // failed far away (a null switch selector, a null field) — or, with the
            // guest's `catch (IllegalArgumentException)` never firing, silently took
            // the success path. Null NAME is the JDK's own explicit NPE.
            // Tested on the raw arg: `str_of` renders a null reference as the TEXT
            // "null", which would report the wrong exception (and the wrong name).
            if matches!(args.get(1), None | Some(Value::Null)) {
                return Err(VmError::Throw(vm.make_exception("java/lang/NullPointerException", "Name is null")));
            }
            let nm = str_of(vm, args.get(1)).unwrap_or_default();
            let v = crate::opcodes::getstatic_resolve(vm, &cls, &nm, "Ljava/lang/Enum;")?;
            if matches!(v, Value::Null) {
                let canonical = cls.replace(['/', '$'], ".");
                return Err(VmError::Throw(vm.make_exception(
                    "java/lang/IllegalArgumentException",
                    &format!("No enum constant {canonical}.{nm}"),
                )));
            }
            Some(v)
        }
        // Boolean.valueOf(String) parses the text; valueOf(boolean)/booleanValue
        // pass the (unboxed) int through. A boxed Boolean is a Value::Int here.
        ("java/lang/Boolean", "valueOf", d) if d.starts_with("(Ljava/lang/String;") => {
            let b = matches!(args.first(), Some(Value::Str(s)) if s.trim().eq_ignore_ascii_case("true"));
            Some(vm.boolean_obj(b))
        }
        // valueOf(boolean) BOXES → a real Boolean object (so instanceof Boolean and
        // gson's JsonPrimitive.isBoolean work); booleanValue() UNBOXES → 0/1.
        ("java/lang/Boolean", "valueOf", _) => Some(vm.boolean_obj(bool_of(vm, args.first()) != 0)),
        ("java/lang/Boolean", "booleanValue", _) => Some(Value::Int(bool_of(vm, args.first()))),
        ("java/lang/Boolean", "<init>", _) => {
            if let Some(Value::Ref(r)) = args.first() {
                let b = match args.get(1) {
                    Some(Value::Str(s)) => s.trim().eq_ignore_ascii_case("true") as i32,
                    other => other.and_then(Value::as_int).unwrap_or(0),
                };
                vm.heap.put_field(*r, "$value", Value::Int(b));
            }
            None
        }
        ("java/lang/Integer", "toUnsignedLong", _) =>
            Some(Value::Long(args.first().and_then(Value::as_int).unwrap_or(0) as u32 as i64)),
        // `Byte.toUnsignedInt(b)` == `b & 0xff`: guava's `UnsignedBytes.toInt` (reached from
        // `RegistryAccess.fromRegistryOfRegistries` via `ImmutableMap` hashing) was the first
        // caller and met an `UnsatisfiedLinkError`. Same shape as the two arms beside it.
        ("java/lang/Byte", "toUnsignedInt", _) =>
            Some(Value::Int(int_of_or0(vm, args.first()) & 0xff)),
        ("java/lang/Short", "toUnsignedInt", _) =>
            Some(Value::Int(int_of_or0(vm, args.first()) & 0xffff)),
        ("java/lang/Integer", "toUnsignedString", _) =>
            Some(Value::Str((args.first().and_then(Value::as_int).unwrap_or(0) as u32).to_string().into())),
        // Reached when the text arrived as a heap String rather than a Value::Str
        // (gson hands LazilyParsedNumber a heap String), so it must obey the same
        // rules as the Value::Str path in `int_static`: no trimming, an explicit
        // range check, and NumberFormatException rather than a silent 0.
        ("java/lang/Integer", "parseInt", _) | ("java/lang/Long", "parseLong", _) => {
            // A NULL argument is its own JDK message — `Integer.parseInt(null)` reads
            // "Cannot parse null string", not `For input string: "null"`, which is
            // what a caller sees for the four-letter TEXT "null" and is a different
            // bug. `str_of` renders both the same, so the null must be caught here.
            if matches!(args.first(), None | Some(Value::Null)) {
                return Err(VmError::Throw(vm.make_exception(
                    "java/lang/NumberFormatException",
                    "Cannot parse null string",
                )));
            }
            let s = str_of(vm, args.first()).unwrap_or_default();
            let radix = args.get(1).and_then(Value::as_int).unwrap_or(10);
            match parse_java_int(&s, radix, name == "parseLong", false) {
                Ok(v) => Some(v),
                Err((class, msg)) => return Err(VmError::Throw(vm.make_exception(class, &msg))),
            }
        }
        ("java/lang/Integer", "toString", "(I)Ljava/lang/String;")
        | ("java/lang/Long", "toString", "(J)Ljava/lang/String;") => {
            Some(Value::Str(args.first().map(Value::display).unwrap_or_default().into()))
        }
        // Double.toString/Float.toString had no arm at all — String.valueOf(double)
        // reached java_dtoa but the direct call raised UnsatisfiedLinkError. Keyed
        // on the OWNER rather than on the Value's tag so a float that arrived
        // widened still gets Float's (shorter) digits.
        ("java/lang/Double", "toString", _) =>
            Some(Value::Str(crate::java_dtoa(args.first().map(Value::as_f64).unwrap_or(0.0)).into())),
        ("java/lang/Float", "toString", _) =>
            Some(Value::Str(crate::java_ftoa(args.first().map(Value::as_f32).unwrap_or(0.0)).into())),
        ("java/lang/Double", "toHexString", _) =>
            Some(Value::Str(crate::java_hex_double(args.first().map(Value::as_f64).unwrap_or(0.0)).into())),
        ("java/lang/Float", "toHexString", _) =>
            Some(Value::Str(crate::java_hex_float(args.first().map(Value::as_f32).unwrap_or(0.0)).into())),
        ("java/lang/Boolean", "parseBoolean", _) => {
            let b = matches!(args.first(), Some(Value::Str(s)) if s.eq_ignore_ascii_case("true"));
            Some(Value::Int(b as i32))
        }
        ("java/lang/Boolean", "getBoolean", _) => {
            let b = property(args.first()).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false);
            Some(Value::Int(b as i32))
        }
        ("java/lang/Integer", "getInteger", _) => {
            Some(property(args.first()).and_then(|v| v.parse::<i32>().ok()).map(Value::Int)
                .unwrap_or_else(|| args.get(1).cloned().unwrap_or(Value::Null)))
        }
        ("java/lang/Long", "getLong", _) => {
            Some(property(args.first()).and_then(|v| v.parse::<i64>().ok()).map(Value::Long)
                .unwrap_or_else(|| args.get(1).cloned().unwrap_or(Value::Null)))
        }
        ("java/lang/Boolean", "toString", _) => {
            Some(Value::Str(if bool_of(vm, args.first()) != 0 { "true" } else { "false" }.into()))
        }
        ("java/lang/Boolean", "hashCode", _) => Some(Value::Int(if bool_of(vm, args.first()) != 0 { 1231 } else { 1237 })),
        // The arm EVERY OBJECT RENDER LANDS ON, and the one the box makes reachable:
        // `String.valueOf(Object)`, `"" + o`, `%s`, `Objects.toString`, a collection's own
        // `toString` and gson's key writer all route a `Value::Ref` through `value_to_string`,
        // which runs the receiver's real `toString` -- which is this. `int_of` rather than
        // `as_int`, because the receiver is now a `Ref` whose payload lives in `$value`; with
        // `as_int` it read `None` and every boxed char printed NUL.
        ("java/lang/Character", "toString", _) => {
            let c = int_of(vm, args.first()).and_then(|n| char::from_u32(n as u32)).unwrap_or('\0');
            Some(Value::Str(c.to_string().into()))
        }
        ("java/lang/Integer" | "java/lang/Long" | "java/lang/Short" | "java/lang/Byte"
        | "java/lang/Double" | "java/lang/Float", "toString", "()Ljava/lang/String;") => {
            // A boxed `Short`/`Byte` receiver is a `Ref`, and `Value::display` of a `Ref` is not
            // its payload -- so the render has to unbox first. The other four owners have no box
            // object at all and take `display` exactly as before.
            match boxed_narrow(vm, args.first()) {
                Some((_, n)) => Some(Value::Str(n.to_string().into())),
                None => Some(Value::Str(args.first().map(Value::display).unwrap_or_default().into())),
            }
        }
        ("java/lang/Boolean", "equals", _) => {
            // JVM semantics: `other instanceof Boolean && value == other.value`.
            // The other operand must actually BE a Boolean — bool_of on an
            // arbitrary object reads 0, which made FALSE.equals(anyObject) true
            // (collapsing guava table keys that mix Boolean and enum values).
            // ⛔THE "raw autoboxed int guard" IS GONE. It accepted ANY `Value::Int` as a
            // Boolean, so `Boolean.TRUE.equals(Integer.valueOf(1))` answered true
            // (bench/EqualsSem.java, `boolVsInt`) — the same textual-equality mistake the
            // numeric arm above made, one variant along. A boxed boolean in this engine is a
            // real heap object from `bool_cache` precisely so `instanceof Boolean` can work,
            // so the Ref arm is the whole population; the control pairs in that probe
            // (`boolVsBool`, `boolFalse`, `boolBoxed`) exist to prove it.
            let other_is_bool = match args.get(1) {
                Some(Value::Ref(h)) => vm.heap.class_of(*h) == Some("java/lang/Boolean"),
                _ => false,
            };
            let eq = other_is_bool && bool_of(vm, args.first()) == bool_of(vm, args.get(1));
            Some(Value::Int(eq as i32))
        }
        ("java/lang/Integer" | "java/lang/Long" | "java/lang/Short" | "java/lang/Byte"
        | "java/lang/Double" | "java/lang/Float" | "java/lang/Character", "equals", _) => {
            // TYPED, not textual. Comparing `a.display() == b.display()` made
            // `Integer.valueOf(1).equals(Long.valueOf(1L))` answer TRUE, because both render
            // as "1" — and `Float.valueOf(1f).equals(Double.valueOf(1.0))` with it. The JVM
            // requires `other instanceof <the receiver's own box>`, which the OWNER names.
            //
            // ⚠What this CANNOT separate is Integer from Short, Byte and Character: this
            // engine's value model has no variant for the narrow boxes, so all four arrive as
            // `Value::Int`. That is the same gap `bench/NumSem.java` declares for
            // `compound.char`, and closing it means giving boxed values a type — a change to
            // the value model rather than to this arm.
            // AND THE GAP THIS ARM DECLARED IS NOW CLOSED. The comment above recorded that
            // `Integer` could not be separated from `Short`, `Byte` and `Character` because all
            // four arrived as `Value::Int` -- "closing it means giving boxed values a type, a
            // change to the value model". That change is `crate::boxid`: the three narrow boxes
            // are real objects of their own class, so the `Ref` arm below decides them exactly,
            // and a `Value::Int` can now only be an `Integer` (or a raw primitive). So the
            // `Value::Int` arm no longer admits the other three -- `Character.valueOf('#')
            // .equals(Integer.valueOf(35))` was TRUE and is now false.
            //
            // Gated: with `RAVA_NO_BOXID=1` no box exists, all four still arrive as `Value::Int`,
            // and admitting them is the only answer that keeps correct guest code working.
            let narrow_boxed = crate::boxid::enabled();
            let kind_ok = match (owner, args.get(1)) {
                (_, Some(Value::Ref(h))) => vm.heap.class_of(*h) == Some(owner),
                ("java/lang/Long", Some(Value::Long(_)))
                | ("java/lang/Double", Some(Value::Double(_)))
                | ("java/lang/Float", Some(Value::Float(_))) => true,
                ("java/lang/Integer", Some(Value::Int(_))) => true,
                (
                    "java/lang/Short" | "java/lang/Byte" | "java/lang/Character",
                    Some(Value::Int(_)),
                ) => !narrow_boxed,
                _ => false,
            };
            // BY PAYLOAD, not by `display`: a box's `display` is its handle rendering, so two
            // equal `Character`s would have compared their HANDLES -- and two distinct boxes for
            // the same out-of-cache-range char would have answered false.
            let render = |vm: &Vm, v: Option<&Value>| -> String {
                match boxed_narrow(vm, v) {
                    Some((_, n)) => n.to_string(),
                    None => v.map(Value::display).unwrap_or_default(),
                }
            };
            let eq = kind_ok && render(vm, args.first()) == render(vm, args.get(1));
            Some(Value::Int(eq as i32))
        }
        // `_` rather than "()I": the STATIC Short.hashCode(short)/Byte.hashCode(byte)
        // have the same body and previously reached no arm at all.
        // `java/lang/Character` JOINS THIS ARM. `Character.hashCode()` is `(int) value` in the
        // JDK, and there was no arm for it at all -- so it resolved up to `Object.hashCode` and
        // answered `identity_hash`, which for the old `Value::Int` representation happened to BE
        // the code point and is now the box's HANDLE. Every `HashMap<Character, ..>` depends on
        // this. `int_of` unboxes; the STATIC `Short.hashCode(short)` forms still land here too
        // (the descriptor is `_`).
        ("java/lang/Integer" | "java/lang/Short" | "java/lang/Byte"
        | "java/lang/Character", "hashCode", _) => {
            Some(Value::Int(int_of_or0(vm, args.first())))
        }
        ("java/lang/Long", "hashCode", _) => {
            let l = args.first().and_then(Value::as_long).unwrap_or(0);
            Some(Value::Int((l ^ (l >> 32)) as i32))
        }
        // Float.hashCode is floatToIntBits — a 32-bit quantity, NOT the folded
        // double bits, so the shared f64 body gave Float.valueOf(1.5f) the wrong
        // hash entirely. Both canonicalise NaN, so every NaN hashes alike whatever
        // payload it carries. `_` on the descriptor also picks up the STATIC
        // Double.hashCode(double)/Float.hashCode(float), which had no arm at all.
        ("java/lang/Double" | "java/lang/Float", "hashCode", _) => {
            Some(Value::Int(if owner == "java/lang/Float" {
                let f = args.first().map(Value::as_f32).unwrap_or(0.0);
                (if f.is_nan() { 0x7FC0_0000 } else { f.to_bits() }) as i32
            } else {
                let d = args.first().map(Value::as_f64).unwrap_or(0.0);
                let b = if d.is_nan() { 0x7FF8_0000_0000_0000 } else { d.to_bits() };
                (b ^ (b >> 32)) as i32
            }))
        }
        // compareTo is compare on the boxed type: the total float order for
        // Double/Float (it answered 0 for both (0.0, -0.0) and any NaN), and INTEGER
        // comparison for the rest — routing a Long through f64 collapsed every pair
        // that differs only past 2^53.
        ("java/lang/Double" | "java/lang/Float", "compareTo", _) => {
            Some(Value::Int(java_dcmp(
                args.first().map(Value::as_f64).unwrap_or(0.0),
                args.get(1).map(Value::as_f64).unwrap_or(0.0),
            )))
        }
        ("java/lang/Integer" | "java/lang/Long" | "java/lang/Short" | "java/lang/Byte"
        | "java/lang/Character" | "java/lang/Boolean", "compareTo", _) => {
            // `long_of`, not `as_i64`: `Value::as_long` answers `None` for a `Ref`, so a boxed
            // `Character`/`Short`/`Byte` operand compared as ZERO -- every `TreeMap<Character,..>`
            // and every `Comparable` sort of them. A `Boolean` box is deliberately NOT in
            // `boxed_narrow`, and does not need to be: `Boolean.compareTo` is reached with the
            // unboxed 0/1 that the `Z`-param rule in `expand_locals_into` delivers, so
            // `long_of`'s `as_long` fallback answers it exactly as `as_i64` did.
            let a = long_of(vm, args.first());
            let b = long_of(vm, args.get(1));
            Some(Value::Int((a > b) as i32 - (a < b) as i32))
        }

        // java.lang.Enum — supers of jar enum classes. <init>(name, ordinal)
        // runs from the enum's <clinit>; ordinal/name read those fields.
        // `enum_{put_,}field` route to a handle (Value::Ref) OR an arena-born enum
        // (Value::Obj, RAVA_ARENA_ENUM) — flag OFF stays Ref-only byte-identical.
        ("java/lang/Enum", "<init>", _) => {
            let recv = args.first().cloned();
            if let Some(n) = args.get(1).cloned() { enum_put_field(vm, recv.as_ref(), "$enumName", n); }
            if let Some(o) = args.get(2).cloned() { enum_put_field(vm, recv.as_ref(), "$enumOrd", o); }
            None
        }
        ("java/lang/Enum", "ordinal", "()I") => {
            Some(enum_field(vm, args.first(), "$enumOrd").unwrap_or(Value::Int(0)))
        }
        ("java/lang/Enum", "name", _) | ("java/lang/Enum", "toString", _) => {
            Some(enum_field(vm, args.first(), "$enumName").unwrap_or(Value::Null))
        }
        // Enum.compareTo: `self.ordinal - other.ordinal`, RAW (JDK source), not its
        // sign. The table had ordinal/name/toString but no compareTo, so a guest enum
        // whose own class declares none resolved up to Enum and died on an
        // UnsatisfiedLinkError — every `Comparable` use of an enum (sort, TreeMap,
        // compareTo in guest code) hit it.
        // Enum.getDeclaringClass: the ENUM TYPE, which differs from getClass() for a
        // constant with a body — `PLUS { ... }` is an anonymous subclass P4$Op$1 and
        // its declaring class is P4$Op. The JDK derives it exactly this way (the
        // superclass, unless that is Enum itself). Missing entirely before, so any
        // `EnumMap`/`EnumSet`-style keying on the declaring class died on a ULE.
        ("java/lang/Enum", "getDeclaringClass", _) => {
            let cls = class_name_of(vm, args.first());
            let decl = match vm.loader.get_or_load(&cls).ok().and_then(|c| c.file.super_class.clone()) {
                Some(s) if s != "java/lang/Enum" => s,
                _ => cls,
            };
            Some(make_class_object(vm, &decl))
        }
        ("java/lang/Enum", "compareTo", _) => {
            let a = enum_field(vm, args.first(), "$enumOrd").and_then(|v| v.as_int()).unwrap_or(0);
            let b = enum_field(vm, args.get(1), "$enumOrd").and_then(|v| v.as_int()).unwrap_or(0);
            Some(Value::Int(a - b))
        }

        // java.lang.Object — every class inherits these.
        ("java/lang/Object", "getClass", "()Ljava/lang/Class;") => {
            let cls = class_name_of(vm, args.first());
            Some(make_class_object(vm, &cls))
        }
        ("java/lang/Object", "hashCode", "()I") => Some(Value::Int(identity_hash(args.first()))),
        ("java/lang/Object", "equals", "(Ljava/lang/Object;)Z") => {
            let eq = ref_eq(args.first(), args.get(1));
            Some(Value::Int(eq as i32))
        }
        ("java/lang/Object", "toString", "()Ljava/lang/String;") => {
            let cls = class_name_of(vm, args.first());
            let h = identity_hash(args.first());
            Some(Value::Str(format!("{}@{:x}", cls.replace('/', "."), h).into()))
        }
        // THE ONLY OTHER EXIT, and the whole warrant for `table1_may_serve` skipping this call:
        // every arm above is keyed on an owner literal, so an owner outside `OWNERS1` can only
        // arrive here.
        _ => return Ok(None),
    };
    Ok(Some(handled))
}

// ═══════════════════════════════════════════════════════════════════════════
// THE CASCADE TAIL IS A LADDER, AND THE TRAVERSALS THAT SERVE NOTHING PAY IT WHOLE.
// ═══════════════════════════════════════════════════════════════════════════
//
// `owner_table` and `owner_table2` are `match (owner, name, descriptor)` over ~73 and ~48 arm
// heads. rustc lowers a match on string literals into an ORDERED chain of `str` compares — there
// is no switch — so a call that reaches the tail pays a length test (and, on a length hit, a
// memcmp) for every arm above the one that answers, and a call that NO arm answers pays every
// arm of BOTH tables. `RAVA_CASCADE_STAT` is what put a number on that population: the last-arm
// shortcut reports `hit=1,867,032 miss=18,011,344` while the served census totals ~10.5 M, so up
// to a THIRD of the traversals walk the entire tail to arrive at `_ => Ok(None)`.
//
// ⭐**AND THE ANSWER FOR THOSE IS DECIDABLE FROM THE OWNER ALONE.** Every arm of `owner_table` is
// keyed on an owner LITERAL, and so is every arm of `owner_table2`. If the owner is in neither
// table's literal set, no arm can match and the only reachable exit is the wildcard — so SKIPPING
// the call IS the call's answer, byte for byte.
//
// ⚠**WHY A FINGERPRINT AND NOT THE OWNER-BITS MEMO.** `owner_bits` answers exactly this shape of
// question, and it is the wrong instrument here for two measured reasons. Its entry is reached
// through an address-keyed direct table that stores a `Box<str>` COPY of the name, so a conflict
// miss ALLOCATES — on a path whose whole complaint (allocation census: `dispatch_owner` 4.1%,
// `try_invoke_chain` 3.5% of sampled allocation stacks) is that a dispatch DECISION allocates at
// all. And the gate built on it, `RAVA_DISPATCH_GATE`, measured **+0.25% CPU** over six
// alternated product boots and stayed opt-in because it is a superset claim over six OTHER
// modules that a future edit can silently break. The filter below is neither: it is stateless
// (no memo, no entry, no allocation, nothing to invalidate) and its literals live in the SAME
// FILE as the match they guard, with a test that reads that file's source and fails the build if
// an arm grows an owner the set does not know.
//
// TOTAL TABLE SIZE: 64 bytes per map, const-evaluated at compile time. Two live here (`MAP1`,
// `MAP2`); two more of the same shape live in `cascade.rs` — `MAP_INLINE` for the inline-arm
// ladder and `MAP_LASTARM` for the last-arm shortcut — so the whole owner-filter family is
// **four cache lines of `.rodata`**, and every one of them asserts its own size in a test. That
// number is tracked here because this tree has measured a **+6.8% REGRESSION** from a front cache
// that grew 2.5 KB -> 14 KB and stopped fitting in L1; a filter that costs four cache lines
// cannot repeat it, and a fifth map has to justify itself against that history.

/// Bits in one owner-filter bitmap. 512 bits against ~20 literals is a ~4% false-positive rate,
/// and a false positive costs exactly the historic scan — the filter may only ever ADMIT too
/// much, never reject something an arm could serve.
pub(crate) const SIG_BITS: u32 = 512;
pub(crate) const SIG_WORDS: usize = (SIG_BITS / 64) as usize;

/// Returned for a name too short to fingerprint, and used when the fast cascade is off: out of
/// range, so [`sig_maybe`] admits unconditionally.
pub(crate) const SIG_ADMIT: u32 = u32::MAX;

/// Fingerprint of an owner name — length plus three bytes, in about eight instructions, with no
/// memo, no lock, no hash of the whole 40-60 byte package path and no allocation.
///
/// It is NOT an identity: two names may share a fingerprint, and then both are admitted and pay
/// the scan they would have paid anyway. The byte positions are chosen for the population this
/// filter exists to REJECT — jar class names, which differ from `java/lang/Integer` in length and
/// in their LAST bytes long before they differ in their first ones.
#[inline]
pub(crate) const fn owner_sig(owner: &str) -> u32 {
    let b = owner.as_bytes();
    let n = b.len();
    if n < 4 {
        return SIG_ADMIT;
    }
    let h = (n as u32).wrapping_mul(0x9E37_79B1)
        ^ (b[n - 1] as u32).wrapping_mul(0x85EB_CA6B)
        ^ (b[n - 2] as u32).wrapping_mul(0xC2B2_AE35)
        ^ (b[n >> 1] as u32).wrapping_mul(0x27D4_EB2F);
    (h >> 13) & (SIG_BITS - 1)
}

/// One bitmap probe. An out-of-range fingerprint (a name shorter than four bytes, or the switch
/// being off) ADMITS — the refusal side must always be the provable one.
#[inline]
pub(crate) fn sig_maybe(map: &[u64; SIG_WORDS], sig: u32) -> bool {
    sig >= SIG_BITS || map[(sig >> 6) as usize] & (1u64 << (sig & 63)) != 0
}

/// Const-evaluated at compile time: no `OnceLock`, no first-call initialisation and no branch on
/// the read path — the map is a plain `.rodata` array.
pub(crate) const fn sig_map(owners: &[&str]) -> [u64; SIG_WORDS] {
    let mut m = [0u64; SIG_WORDS];
    let mut i = 0;
    while i < owners.len() {
        let s = owner_sig(owners[i]);
        // A literal shorter than four bytes cannot be fingerprinted and would have to be admitted
        // unconditionally. No owner in either table is that short; this assert (a compile-time
        // error, since the map is const-evaluated) is what keeps it that way.
        assert!(s < SIG_BITS, "owner literal too short to fingerprint");
        m[(s >> 6) as usize] |= 1u64 << (s & 63);
        i += 1;
    }
    m
}

/// EVERY owner literal `owner_table`'s match keys an arm on. This list is the only thing standing
/// between an arm and the filter that guards it, so it is checked against this file's own source
/// by `no_class_literal_in_this_file_is_unregistered` below rather than by review.
pub(crate) const OWNERS1: &[&str] = &[
    "io/netty/channel/unix/ErrorsStaticallyReferencedJniMethods",
    "java/util/Timer",
    "java/awt/GraphicsEnvironment",
    "java/lang/System",
    "java/lang/Runtime",
    "java/lang/Double",
    "java/lang/Float",
    "java/lang/Long",
    "java/lang/Integer",
    "java/lang/Short",
    "java/lang/Byte",
    "java/lang/Character",
    "java/lang/Boolean",
    "java/lang/Number",
    "java/lang/Enum",
    "java/lang/Object",
];

/// The drift test reads only the part of this file ABOVE the first occurrence of this text — the
/// match itself. Everything below it is the filter, its literal lists and its tests, whose
/// class-shaped strings are fixtures and examples rather than arm keys.
#[cfg(test)]
const FILTER_SECTION_MARKER: &str = "THE CASCADE TAIL IS A LADDER";

/// Class-shaped literals in this file that are NOT owner keys — exception names a body raises and
/// classes a body allocates. Listed so the drift test can tell "not an owner" from "an owner
/// nobody registered": a NEW literal must be classified by whoever adds it, and until it is, the
/// build fails.
#[cfg(test)]
const NOT_OWNERS1: &[&str] = &[
    "java/lang/IllegalArgumentException",
    "java/lang/NullPointerException",
    "java/lang/NumberFormatException",
    "java/lang/UnsatisfiedLinkError",
    "java/util/HashMap",
    "java/util/Properties",
];

static MAP1: [u64; SIG_WORDS] = sig_map(OWNERS1);

/// Can any arm of [`owner_table`] be keyed on this owner?
///
/// `false` is a PROOF OF ABSENCE: the owner is in none of the table's literal sets, the table has
/// no owner-agnostic and no guarded-binder arm, and its only other exit is `_ => return
/// Ok(None)`. The caller may therefore skip the call entirely — that is not an approximation of
/// the answer, it IS the answer. `true` says nothing, and the table runs exactly as before.
#[inline]
pub(crate) fn table1_may_serve(sig: u32) -> bool {
    sig_maybe(&MAP1, sig)
}

/// DEFAULT-ON. `RAVA_NO_FAST_CASCADE=1` restores the unfiltered tail — both tables run for every
/// owner, exactly as they did — so a boot that goes wrong bisects this whole family in one run.
/// Also consults `emit_more_disabled`, so it falls together with the other emit-more switches.
pub(crate) fn fast_cascade() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_FAST_CASCADE").is_none()
    })
}

/// `RAVA_FAST_CASCADE_VERIFY` (default OFF, inert): run a tail table the filter REJECTED anyway
/// and report any arm that would have served it. The filter is a claim about a match statement's
/// literal set, and this is how an edit that breaks the claim gets caught on a real workload
/// instead of in review — the discipline `RAVA_DISPATCH_GATE_VERIFY` applies to the stages.
pub(crate) fn fast_cascade_verify() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FAST_CASCADE_VERIFY").is_some())
}

/// Report a tail table that served an owner the filter had excluded. Only reachable under
/// `RAVA_FAST_CASCADE_VERIFY`, which runs the rejected table regardless.
#[cold]
pub(crate) fn fast_cascade_miss(table: &str, owner: &str, name: &str, descriptor: &str) {
    FAST_CASCADE_MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    eprintln!("[FAST-CASCADE] MISS: {table} served {owner}.{name}{descriptor}");
}

/// Verify-mode tally, so "no misses" can be told apart from "the verify mode never ran" — the
/// distinction four dead instruments in this ledger were read without.
pub(crate) static FAST_CASCADE_MISSES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Every class-shaped string literal in a Rust source file. Deliberately crude and deliberately
/// OVER-inclusive: a literal it wrongly picks up only has to be classified once, whereas a
/// literal it MISSED would be an arm the filter is free to skip. Escaped strings are dropped —
/// no class name contains a backslash.
#[cfg(test)]
pub(crate) fn class_literals(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut k = 0usize;
    while k < b.len() {
        if b[k] != b'"' {
            k += 1;
            continue;
        }
        let start = k + 1;
        let mut j = start;
        let mut esc = false;
        while j < b.len() && b[j] != b'"' {
            if b[j] == b'\\' {
                esc = true;
                j += 1;
            }
            j += 1;
        }
        if !esc && j <= b.len() && src.is_char_boundary(start) && src.is_char_boundary(j) {
            let s = &src[start..j];
            let ok = s.len() > 3
                && s.contains('/')
                && !s.starts_with('/')
                && !s.ends_with('/')
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c == b'/');
            if ok && !out.iter().any(|e| e == s) {
                out.push(s.to_string());
            }
        }
        k = j + 1;
    }
    out
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    /// Every registered owner must pass its own filter — the one direction in which a bug here is
    /// silent wrong execution rather than lost speed.
    #[test]
    fn every_registered_owner_is_admitted() {
        for o in OWNERS1 {
            assert!(table1_may_serve(owner_sig(o)), "{o} rejected by its own filter");
        }
    }

    /// ⭐**THE DRIFT CHECK READS THE TABLE'S OWN SOURCE.** The filter's whole safety argument is
    /// that `OWNERS1` lists every owner `owner_table`'s match keys an arm on. Review cannot hold
    /// that invariant across future edits; this can. A newly added arm's owner is a class-shaped
    /// literal in this file, and this test fails the build until it is classified as an owner
    /// (`OWNERS1`) or as something a body merely names (`NOT_OWNERS1`).
    #[test]
    fn no_class_literal_in_this_file_is_unregistered() {
        let src = include_str!("cascade_table.rs");
        let body = src.split(FILTER_SECTION_MARKER).next().unwrap_or(src);
        for lit in class_literals(body) {
            let known = OWNERS1.contains(&lit.as_str()) || NOT_OWNERS1.contains(&lit.as_str());
            assert!(
                known,
                "class literal {lit:?} appears in cascade_table.rs but is in neither OWNERS1 nor \
                 NOT_OWNERS1. If it is the owner of a new match arm it MUST go in OWNERS1, or \
                 `table1_may_serve` will skip that arm for every call."
            );
        }
    }

    /// A jar class must be REJECTED — otherwise the filter admits everything and buys nothing.
    /// Not a correctness assertion (admitting is always safe); a smoke test that the fingerprint
    /// discriminates at all.
    #[test]
    fn representative_jar_classes_are_rejected() {
        let rejected = [
            "net/minecraft/world/level/levelgen/NoiseChunk",
            "net/minecraft/core/Holder$Reference",
            "it/unimi/dsi/fastutil/objects/Object2ObjectOpenHashMap",
            "com/mojang/serialization/codecs/RecordCodecBuilder",
        ]
        .iter()
        .filter(|o| !table1_may_serve(owner_sig(o)))
        .count();
        assert!(rejected >= 3, "the fingerprint is not discriminating: only {rejected}/4 rejected");
    }

    /// The map is `.rodata`, not a cache — the byte size the module comment states, asserted.
    #[test]
    fn the_map_is_one_cache_line() {
        assert_eq!(std::mem::size_of_val(&MAP1), 64);
    }
}
