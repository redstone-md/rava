//! Fields, methods, constructors and the signature parser behind them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

/// ONE PROCESS WORD FOR THIS FILE'S GATES, read once at the natural entry of a reflective call
/// and passed down.
///
/// WAS: three separate process-constant reads on a single `Method.invoke` —
/// `fast_reflect()`'s `OnceLock<bool>` (inside `resolve_member`), plus
/// `jit::env_gate("RAVA_TRACE_THROW", &TT)`, which is itself TWO reads: `env_memo_enabled()`
/// (`gate(HG_ENV_MEMO)`) and then the per-variable `OnceLock`. Each `get_or_init` is an
/// `Acquire` load of the once-state, a compare, a branch and a dependent load of the payload —
/// and an acquire load is an OPTIMISATION BARRIER LLVM may not hoist out of the caller's loop,
/// so it also forces the surrounding values to be materialised at each one. IS: one acquire
/// load of a byte at the top of the reflective entry point, then register tests.
///
/// The same shape as `natives::entry::entry_gates`, `opcodes::gates::op_gates` and
/// `jit::hot_gates`, and for the same reason.
///
/// ⚠ `RAVA_TRACE_THROW` no longer honours `RAVA_NO_ENV_MEMO` on THIS path (it still does
/// everywhere else `env_gate` is used). That switch exists to let a debug run change a gate
/// mid-process by re-reading the environment; this gate only turns on an `eprintln!`, so
/// freezing it at first use costs nothing a debugger can want, and a raw
/// `RtlQueryEnvironmentVariable` — a UTF-16 encode of the key, a linear walk of the whole
/// process environment block under a process-wide lock — per reflective invoke is exactly the
/// shape this round exists to remove.
#[derive(Clone, Copy)]
pub(crate) struct ReflGates(u8);

/// `RAVA_NO_FAST_REFLECT` clear: memoise (class, member, argdesc) resolutions.
const RG_FAST: u8 = 1 << 0;
/// `RAVA_NO_FAST_REFLSHAPE` clear: read a mirror's `$fname` in the arms that use it instead of
/// in the prologue (see `field_method`).
const RG_SHAPE: u8 = 1 << 1;
/// `RAVA_TRACE_THROW` set: log every reflective invoke and whether it resolved.
const RG_TRACE_THROW: u8 = 1 << 2;

impl ReflGates {
    /// The resolution memo, and with it the second `getModifiers` scan it removed.
    #[inline]
    pub(crate) fn fast(self) -> bool {
        self.0 & RG_FAST != 0
    }
    /// This round's lazy mirror-field reads.
    #[inline]
    fn shape(self) -> bool {
        self.0 & RG_SHAPE != 0
    }
    #[inline]
    fn trace_throw(self) -> bool {
        self.0 & RG_TRACE_THROW != 0
    }
}

/// The word. One `OnceLock`, one environment sweep, at first reflective call.
#[inline]
pub(crate) fn refl_gates() -> ReflGates {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    ReflGates(*W.get_or_init(|| {
        // `RAVA_NO_EMIT_MORE` folds the whole round, exactly as the two gates did separately.
        let round = !crate::flags::emit_more_disabled();
        let mut w = 0u8;
        if round && std::env::var_os("RAVA_NO_FAST_REFLECT").is_none() {
            w |= RG_FAST;
        }
        if round && std::env::var_os("RAVA_NO_FAST_REFLSHAPE").is_none() {
            w |= RG_SHAPE;
        }
        if std::env::var_os("RAVA_TRACE_THROW").is_some() {
            w |= RG_TRACE_THROW;
        }
        w
    }))
}

/// `RAVA_NO_FAST_REFLECT=1` opts out of this file's sixth-round rewrite (DEFAULT ON), and it
/// also folds under the round-wide `RAVA_NO_EMIT_MORE`.
///
/// What it gates: the (class, member, argdesc) resolution memo below, and — with it — the
/// second scan `getModifiers` used to make over the declaring class's method table. The
/// allocation removals in this file are not gated because they cannot change an answer: an
/// `RStr` read instead of a `String` copy, and a descriptor sliced instead of re-`format!`ed,
/// return the same bytes by construction.
///
/// Kept as a named accessor for the one out-of-file caller (`reflect::futures`); inside this
/// file the gates arrive as `ReflGates`, read once per call, so nothing asks twice.
pub(crate) fn fast_reflect() -> bool {
    refl_gates().fast()
}

/// A synthetic mirror field as the `RStr` the heap already holds.
///
/// `field_str`/`strfield` (reflect/mod.rs) answer the same question as a `String` — i.e. they
/// MALLOC AND COPY the name, the owner and the descriptor on EVERY reflective call, for values
/// every caller only reads. `RStr` is an `Arc<String>`, so the clone `get_field` has already
/// performed is the entire cost of this form; the copy on top was pure waste.
///
/// WAS: 3 heap allocations per `Method` call and 3 per `Field` call (owner + name + descriptor),
/// growing with reflective calls per boot. IS: 0.
#[inline]
fn field_rstr(vm: &Vm, h: u32, f: &str) -> Option<crate::value::RStr> {
    match vm.heap.get_field(h, f) {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

/// `field_rstr` reading through an argument slot (the mirror is always `args[0]`).
#[inline]
fn arg_rstr(vm: &Vm, v: Option<&Value>, f: &str) -> Option<crate::value::RStr> {
    match v {
        Some(Value::Ref(h)) => field_rstr(vm, *h, f),
        _ => None,
    }
}

/// A per-thread cached `RStr` for a fixed literal, so a DEFAULTED mirror field costs a refcount
/// bump instead of a fresh `Arc<String>` on every call that misses.
macro_rules! lit_rstr {
    ($s:literal) => {{
        thread_local! { static L: crate::value::RStr = crate::value::RStr::from($s); }
        L.with(|v| v.clone())
    }};
}

/// The empty `RStr`, so an absent mirror field costs no allocation either.
#[inline]
fn empty_rstr() -> crate::value::RStr {
    lit_rstr!("")
}

/// The declaring-class field id for `field` on `target`'s OWN class, without building the class
/// name as a `String` first.
///
/// `Vm::value_class_name` returns `String`, so every atomic field-updater operation — and
/// jctools' MPSC `offer()` spins one in a CAS loop — allocated and copied the receiver's class
/// name only to hash it and drop it. Both sources already own the bytes (`Heap::class_of`
/// borrows, `nextgen::class_name` hands back an `Arc<str>`), so the id resolves straight off
/// them. WAS: one allocation per CAS attempt. IS: none.
#[inline]
fn target_field_id(vm: &Vm, target: &Value, field: &str) -> u32 {
    match target {
        Value::Ref(h) => vm.resolve_field_id(vm.heap.class_of(*h).unwrap_or(""), field),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => match crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)) {
            Some(n) => vm.resolve_field_id(&n, field),
            None => vm.resolve_field_id("", field),
        },
        _ => vm.resolve_field_id("", field),
    }
}

/// How many parameters a BARE descriptor list declares — `descriptor::arg_count`'s walk (`[`
/// binds to the element type that follows, `L` runs to the next `;`, anything else is one
/// character), minus the parentheses it scans for.
///
/// A `Method` mirror stores its parameter list WITHOUT the surrounding `(...)`, so asking the
/// shared counter meant `format!("({ad})V")` — building a whole descriptor, per call, so that a
/// counter could immediately discard the two characters that were added.
fn count_param_descs(s: &str) -> usize {
    let b = s.as_bytes();
    let (mut i, mut n) = (0usize, 0usize);
    // Byte-for-byte `descriptor::arg_count`'s loop, entered past the `(` it would have sought
    // and still stopping at a `)` — so a mirror whose `$argdesc` somehow carried one answers
    // exactly what the wrapped form answered.
    while i < b.len() && b[i] != b')' {
        match b[i] {
            b'[' => { i += 1; continue; } // array marker binds to the element type that follows
            b'L' => { while i < b.len() && b[i] != b';' { i += 1; } i += 1; }
            _ => i += 1, // primitive: single char
        }
        n += 1;
    }
    n
}

/// Everything a resolved reflective member answers about its (class, name, argdesc) triple.
///
/// `access` is the REAL `ACC_*` word, resolved once. `getModifiers` used to re-derive it with a
/// SECOND linear scan of `decl.file.methods` immediately after `resolve_by_args` had already
/// found that very method — the scan grows with methods per class, and Spigot's `readConfig`
/// plus gson's `Excluder` ask it of every declared member.
#[derive(Clone)]
struct MemberFacts {
    decl: std::sync::Arc<crate::class_loader::LoadedClass>,
    desc: std::sync::Arc<str>,
    is_static: bool,
    access: i32,
}

/// Interned resolutions, keyed by the FULL triple and bucketed by its FxHash.
///
/// THREAD-LOCAL ON PURPOSE. The fact memoised — "name X resolves to class identity Y, which
/// declares this descriptor" — is process-global truth (`ClassLoader::define_synthetic`,
/// GAP-S08: one textual name maps to exactly one class identity for the process lifetime), so a
/// shared table would be sound; but sharing it costs an atomic refcount on one cache line per
/// hit and this engine has already measured that trade going the wrong way. It also preserves
/// the GIL-free lane's behaviour exactly: a worker that cannot load a class still takes
/// `resolve_by_args`'s `RegionAbort` because its own memo is empty.
///
/// TABLE SIZE: capped at `MEMO_CAP` = 4096 buckets. An entry is three `Box<str>` headers (48 B)
/// plus two pointers and two words of facts (~32 B) plus the key bytes (~60 B typical) ≈ 140 B,
/// so a saturated table is ~560 KB per thread — but the MC boot resolves on the order of a few
/// hundred distinct triples, i.e. ~40 KB. This is NOT a per-opcode table: it is consulted only
/// from `java.lang.reflect`, so L1 residency is not the criterion its size has to meet.
thread_local! {
    static MEMBER_MEMO: std::cell::RefCell<
        crate::FastMap<u64, Vec<((Box<str>, Box<str>, Box<str>), MemberFacts)>>,
    > = std::cell::RefCell::new(crate::FastMap::default());
}
const MEMO_CAP: usize = 4096;

fn triple_hash(a: &str, b: &str, c: &str) -> u64 {
    use std::hash::Hasher;
    let mut h = crate::FxHasher::default();
    h.write(a.as_bytes());
    h.write(b.as_bytes());
    h.write(c.as_bytes());
    h.finish()
}

/// `Vm::resolve_by_args` plus the access word, memoised on the triple.
///
/// WAS: per reflective CALL, a walk of the receiver's superclass chain with a LINEAR SCAN of
/// `class.file.methods` at each level — O(super depth x methods per class) — plus, for
/// `getModifiers`, a second full scan of the class that just answered. IS: one FxHash of the
/// triple and a key compare, on every call after the first for that (class, member, argdesc).
///
/// ⚠**ONLY POSITIVE ANSWERS ARE MEMOISED, AND THAT IS THE PRECONDITION.** A class identity, once
/// published, never changes — so a member that resolved stays resolved to the same declaration.
/// A MISS is not stable: `ClassLoader::invalidate_for_new_source` exists precisely because a new
/// classpath source can make a previously-absent name resolve, and a URLClassLoader is built
/// mid-boot. So a negative falls through to the real walk every time, which is also what keeps
/// the absent-member exception (`UnsupportedOperationException` from `invoke`, `ACC_PUBLIC` from
/// `getModifiers`) identical to the un-memoised path.
///
/// `g` ARRIVES FROM THE CALLER instead of being read here: `getModifiers`, `getReturnType`
/// and `invoke` each reach this function once per reflective call, and `invoke` reached it
/// after already paying two more process-constant reads for the throw trace. See `refl_gates`.
fn resolve_member(
    vm: &mut Vm,
    g: ReflGates,
    cls: &str,
    mname: &str,
    ad: &str,
) -> Result<Option<MemberFacts>, VmError> {
    let memoise = g.fast();
    let h = if memoise { triple_hash(cls, mname, ad) } else { 0 };
    if memoise {
        let hit = MEMBER_MEMO.with(|m| {
            m.borrow().get(&h).and_then(|bucket| {
                bucket
                    .iter()
                    .find(|((a, b, c), _)| &**a == cls && &**b == mname && &**c == ad)
                    .map(|(_, v)| v.clone())
            })
        });
        if hit.is_some() {
            return Ok(hit);
        }
    }
    let Some((decl, desc, is_static)) = vm.resolve_by_args(cls, mname, ad)? else {
        return Ok(None);
    };
    // The one scan that used to run per `getModifiers` call now runs per DISTINCT member.
    let access = decl
        .file
        .methods
        .iter()
        .find(|m| m.name == mname && m.descriptor == desc)
        .map(|m| m.access_flags as i32)
        .unwrap_or(1);
    let facts = MemberFacts { decl, desc: std::sync::Arc::from(desc.as_str()), is_static, access };
    if memoise {
        MEMBER_MEMO.with(|m| {
            let mut m = m.borrow_mut();
            // REFUSE past the cap rather than grow without bound: a boot that reflects over an
            // unbounded name space keeps working, it just stops being memoised.
            if m.len() < MEMO_CAP {
                m.entry(h).or_default().push((
                    (cls.into(), mname.into(), ad.into()),
                    facts.clone(),
                ));
            }
        });
    }
    Ok(Some(facts))
}

/// The mirror's `$fname`, resolved where it is USED.
///
/// `$eager` is `Some` only under the kill switch, where the prologue probed it as before; on
/// the default path it is `None` and the probe happens here, in the arm that needs the name.
/// The absent-`$fname` answer is the prologue's, unchanged and in every arm: `Null`.
///
/// `take()` rather than `clone()` so the KILL-SWITCH arm is byte-for-byte the old cost — one
/// prologue probe and a move — and the A/B of `RAVA_NO_FAST_REFLSHAPE` therefore prices the
/// removal and not an extra refcount bump charged to the OFF side. Exactly one match arm runs
/// per call, so there is no second reader to find the slot empty.
macro_rules! fname_of {
    ($vm:expr, $eager:expr, $recv:expr) => {
        match $eager.take() {
            Some(f) => f,
            None => match field_rstr($vm, $recv, "$fname") {
                Some(f) => f,
                None => return Ok(Some(Some(Value::Null))),
            },
        }
    };
}

/// java.lang.reflect.Field — get/set a named field (instance or static).
pub(crate) fn field_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    let g = refl_gates();
    // ⭐**`$class` AND `$fdesc` USED TO BE READ ON EVERY CALL, FOR THE THREE ARMS THAT WANT
    // THEM.** gson's `Excluder` asks `getName`, `isSynthetic`, `getModifiers` and
    // `getAnnotation` of every declared field of every type it (de)serialises — the whole
    // datapack reload runs through it — and each of those answers needs neither the owner nor
    // the descriptor. The old prologue still paid two by-name heap probes and two `String`
    // copies for them. They are now read inside the arms that use them.
    //
    // ⭐**AND `$fname` WAS THE ONE LEFT BEHIND — for THREE of those same four questions.**
    // `isSynthetic`, `getModifiers` and `getAnnotation` never look at the name, yet every one
    // of them paid the full by-name mirror read to reach its constant: `field_id_existing`
    // folds `"$fname"` and probes the interner L1, the object's id array is scanned for the
    // slot, and `decode_field` hands back a CLONE of the `Arc<String>` — an atomic increment
    // now and an atomic decrement plus a zero-test when the local dies. Four of those per
    // field on the Excluder path, one of which wanted the answer. It is now read in the arms
    // that read it: `getName`, the eight typed `get*`s and the nine `set*`s.
    //
    // POPULATION AND PROOF: every `java/lang/reflect/Field` in this process is built by
    // `reflect::mod::make_field`, which is the only allocation site of that class name in the
    // tree and always writes `$fname` — so on the default path no arm can observe a mirror
    // without one. `RAVA_NO_FAST_REFLSHAPE=1` restores the eager prologue probe, which is what
    // makes this a switchable removal rather than a rearrangement.
    let mut eager = if g.shape() {
        None
    } else {
        match field_rstr(vm, recv, "$fname") {
            Some(f) => Some(f),
            None => return Ok(Some(Some(Value::Null))),
        }
    };
    let target = args.get(1).cloned();
    let read = |vm: &mut Vm, fname: &crate::value::RStr| -> Result<Value, VmError> {
        let cls = field_rstr(vm, recv, "$class").unwrap_or_else(empty_rstr);
        match &target {
            Some(Value::Ref(o)) => {
                // Same declaring-class storage key as bytecode getfield.
                let id = vm.resolve_field_id(&cls, fname);
                Ok(vm.heap.get_field_hint(*o, id, 0).0)
            }
            _ => { // static
                if let Ok(c) = vm.loader.get_or_load(&cls) { vm.ensure_init(&c)?;
                    return Ok(c.statics.read().unwrap_or_else(|e| e.into_inner()).get(&**fname).cloned().unwrap_or(Value::Null)); }
                Ok(Value::Null)
            }
        }
    };
    let ret = match name {
        // The heap already owns this name as an `Arc<String>`; handing back a refcount bump is
        // the whole operation. It used to malloc a copy of the name to answer "what is the name".
        "getName" => Value::Str(fname_of!(vm, eager, recv)),
        "isSynthetic" | "isEnumConstant" | "isAccessible" | "canAccess" => {
            // gson's Excluder reflects every field to decide what to serialize.
            // We expose only real declared fields, so none are synthetic; access
            // control is not enforced (always accessible).
            Value::Int((name == "isAccessible" || name == "canAccess") as i32)
        }
        "getModifiers" => Value::Int(1),
        "getType" | "getGenericType" => {
            let fdesc = field_rstr(vm, recv, "$fdesc").unwrap_or_else(empty_rstr);
            let t = desc_token_to_class(&fdesc);
            vm.make_class(&t)
        }
        "getAnnotation" | "getDeclaredAnnotation" => Value::Null,
        "isAnnotationPresent" => Value::Int(0),
        "getAnnotations" | "getDeclaredAnnotations" => Value::Array(vm.heap.alloc_values("Ljava/lang/annotation/Annotation;", Vec::new())),
        "setAccessible" | "trySetAccessible" => return Ok(Some(None)),
        // Each typed reader takes the name it actually needs; the widening/narrowing is
        // unchanged, only the point at which `$fname` is fetched moved into these arms.
        "get" => { let f = fname_of!(vm, eager, recv); read(vm, &f)? }
        "getInt" | "getShort" | "getByte" | "getChar" => { let f = fname_of!(vm, eager, recv); Value::Int(read(vm, &f)?.as_i32()) }
        "getLong" => { let f = fname_of!(vm, eager, recv); Value::Long(read(vm, &f)?.as_i64()) }
        "getBoolean" => { let f = fname_of!(vm, eager, recv); Value::Int((read(vm, &f)?.as_i32() != 0) as i32) }
        "getFloat" => { let f = fname_of!(vm, eager, recv); Value::Float(read(vm, &f)?.as_f32()) }
        "getDouble" => { let f = fname_of!(vm, eager, recv); Value::Double(read(vm, &f)?.as_f64()) }
        "set" | "setInt" | "setLong" | "setBoolean" | "setFloat" | "setDouble" | "setShort" | "setByte" | "setChar" => {
            let v = args.get(2).cloned().unwrap_or(Value::Null);
            let fname = fname_of!(vm, eager, recv);
            let cls = field_rstr(vm, recv, "$class").unwrap_or_else(empty_rstr);
            match &target {
                Some(Value::Ref(o)) => {
                    // Reflection is the one writer that can set a FINAL instance field - the
                    // verifier stops bytecode from doing it outside `<init>`. The cross-call
                    // stringify cache admits objects on exactly that guarantee, so this write has
                    // to invalidate it.
                    crate::stringify_memo::bump_immut_epoch();
                    let id = vm.resolve_field_id(&cls, &fname);
                    vm.heap.put_field_hint(*o, id, v, 0);
                }
                // The statics map is keyed by `String`, so the ONE path that genuinely needs an
                // owned name is the one that stores it.
                _ => { if let Ok(c) = vm.loader.get_or_load(&cls) { let _ = vm.ensure_init(&c); vm.bump_static_gen(); c.statics.write().unwrap_or_else(|e| e.into_inner()).insert(fname.to_string(), v); } }
            }
            return Ok(Some(None));
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

pub(crate) fn method_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    // THE NATURAL ENTRY. Every arm below that resolves a member needs the memo gate, and the
    // `invoke` arm used to add two more acquire loads of its own for a debug trace; one byte
    // now answers all three. See `refl_gates`.
    let g = refl_gates();
    let ret = match name {
        "invoke" => {
            // invoke(Object target, Object... args)
            let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
            let Some(cls) = field_rstr(vm, recv, "$class") else { return Ok(Some(Some(Value::Null))) };
            let mname = field_rstr(vm, recv, "$mname").unwrap_or_else(empty_rstr);
            let ad = field_rstr(vm, recv, "$argdesc").unwrap_or_else(empty_rstr);
            let target = args.get(1).cloned().unwrap_or(Value::Null);
            let call_args = crate::natives::varargs(vm, args.get(2));
            // MEMOISED GATE. `RtlQueryEnvironmentVariable` takes a PROCESS-WIDE lock on the environment
            // block, so an unmemoised read here does not just cost its own time - it SERIALISES every
            // lane worker on that lock. The growth-ranked profile (4 vs 6 workers) shows exactly that
            // shape: `std::env::_var_os` 2.58% and `RtlQueryEnvironmentVariable` 2.56% appear only at
            // the wider width, next to `RtlWaitOnAddress` and `ZwWaitForAlertByThreadId` at 5.06% each.
            //
            // AND IT IS NO LONGER A GATE READ AT ALL AT THIS POINT. `env_gate` memoised the
            // variable but still cost TWO acquire loads per invoke — `env_memo_enabled()` and
            // then its own `OnceLock` — and the resolution below cost a third. All three are
            // now bits of `g`, loaded once at the top of this function.
            if g.trace_throw() {
                let r = resolve_member(vm, g, &cls, &mname, &ad)?.is_some();
                eprintln!("[INVOKE] {}.{} argdesc={:?} resolved={r}", &*cls, &*mname, &*ad);
            }
            match resolve_member(vm, g, &cls, &mname, &ad)? {
                Some(f) => {
                    let mut locals = Vec::new();
                    if !f.is_static {
                        locals.push(target);
                    }
                    locals.extend(call_args);
                    vm.invoke(f.decl, &mname, &f.desc, locals)?.unwrap_or(Value::Null)
                }
                // No bytecode: the target is a native-backed JDK method (e.g.
                // ProcessHandle.current(), reached reflectively by netty's
                // DefaultChannelId.defaultProcessId). Route through native
                // dispatch. If even that is unhandled, raise a *catchable*
                // exception rather than a hard VmError — reflective feature
                // probes wrap invoke() in catch(Throwable) and fall back, so a
                // hard error would wrongly abort the boot.
                None => {
                    let mut locals = Vec::new();
                    let native_static = matches!(target, Value::Null);
                    if !native_static { locals.push(target.clone()); }
                    locals.extend(call_args);
                    let desc = format!("({})V", &*ad);
                    match crate::natives::try_invoke(vm, &cls, &mname, &desc, &locals)? {
                        Some(ret) => ret.unwrap_or(Value::Null),
                        None => return Err(VmError::Throw(vm.make_exception(
                            "java/lang/UnsupportedOperationException",
                            &format!("no implementation for {}.{}({})", &*cls, &*mname, &*ad)))),
                    }
                }
            }
        }
        // The name is already an `Arc<String>` on the mirror: a refcount bump, not a copy.
        "getName" => Value::Str(arg_rstr(vm, args.first(), "$mname").unwrap_or_else(empty_rstr)),
        "getModifiers" => {
            let cls = arg_rstr(vm, args.first(), "$class").unwrap_or_else(empty_rstr);
            let mname = arg_rstr(vm, args.first(), "$mname").unwrap_or_else(empty_rstr);
            let ad = arg_rstr(vm, args.first(), "$argdesc").unwrap_or_else(empty_rstr);
            // The REAL access flags (ACC_PRIVATE/STATIC/...). Spigot's readConfig
            // selects methods via Modifier.isPrivate(getModifiers()), so a blanket
            // PUBLIC made it skip every private config method (replaceCommands etc.).
            //
            // The flags now come OUT of the resolution instead of being re-derived by a second
            // scan of the class that just produced it (see `MemberFacts`). Unresolvable still
            // answers ACC_PUBLIC, unchanged.
            match resolve_member(vm, g, &cls, &mname, &ad)? {
                Some(f) => Value::Int(f.access),
                None => Value::Int(1),
            }
        }
        "getParameterCount" => {
            let ad = arg_rstr(vm, args.first(), "$argdesc").unwrap_or_else(empty_rstr);
            // WAS: `format!("({ad})V")` — a whole descriptor built, per call, so a counter
            // could immediately throw away the two characters that were added. The mirror
            // stores the parameter list on its own, so count it where it lies.
            Value::Int(count_param_descs(&ad) as i32)
        }
        // getReturnType()/getParameterTypes() — needed by reflective config
        // loaders (Spigot's SpigotConfig.readConfig filters declared methods by
        // `getReturnType() == Void.TYPE && getParameterTypes().length == 0`, then
        // invokes each; without these, NO config method ran and a static Set field
        // stayed null → NPE in the first server tick). The Method object only
        // stores the arg descriptor, so resolve the method to recover its return.
        "getReturnType" | "getGenericReturnType" => {
            let cls = arg_rstr(vm, args.first(), "$class").unwrap_or_else(empty_rstr);
            let mname = arg_rstr(vm, args.first(), "$mname").unwrap_or_else(empty_rstr);
            let ad = arg_rstr(vm, args.first(), "$argdesc").unwrap_or_else(empty_rstr);
            // The return descriptor is a SLICE of the resolved descriptor; the old form copied
            // it into a `String` on the way to `desc_token_to_class`, which copies again.
            let t = match resolve_member(vm, g, &cls, &mname, &ad)? {
                Some(f) => desc_token_to_class(f.desc.rsplit(')').next().unwrap_or("V")),
                None => desc_token_to_class("Ljava/lang/Object;"),
            };
            vm.make_class(&t)
        }
        "getParameterTypes" | "getGenericParameterTypes" => {
            let ad = arg_rstr(vm, args.first(), "$argdesc").unwrap_or_else(empty_rstr);
            let toks = split_param_descs(&ad);
            let mut objs = Vec::with_capacity(toks.len());
            for d in &toks { let t = desc_token_to_class(d); objs.push(vm.make_class(&t)); }
            Value::Array(vm.heap.alloc_values("Ljava/lang/Class;", objs))
        }
        "getExceptionTypes" => Value::Array(vm.heap.alloc_values("Ljava/lang/Class;", Vec::new())),
        "getDeclaringClass" => {
            let cls = arg_rstr(vm, args.first(), "$class").unwrap_or_else(empty_rstr);
            vm.make_class(&cls)
        }
        "isSynthetic" | "isBridge" | "isVarArgs" | "isDefault" => Value::Int(0),
        "isAnnotationPresent" => Value::Int(0),
        "getAnnotation" | "getDeclaredAnnotation" => Value::Null,
        "getAnnotations" | "getDeclaredAnnotations" => {
            Value::Array(vm.heap.alloc_values("Ljava/lang/annotation/Annotation;", Vec::new()))
        }
        "setAccessible" | "trySetAccessible" => return Ok(Some(None)),
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

pub(crate) fn field_updater(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    if name == "newUpdater" {
        // The field name is the last String argument. It goes straight back onto the updater as
        // the SAME `Arc<String>` the caller handed us — the old form copied it out to a `String`
        // and then allocated a second `Arc` around the copy.
        let field = args.iter().rev().find_map(|a| match a { Value::Str(s) => Some(s.clone()), _ => None }).unwrap_or_else(empty_rstr);
        let h = vm.heap.alloc("java/util/concurrent/atomic/FieldUpdater");
        vm.heap.put_field(h, "$field", Value::Str(field));
        return Ok(Some(Some(Value::Ref(h))));
    }
    let upd = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    let Some(field) = field_rstr(vm, upd, "$field") else { return Ok(Some(Some(Value::Null))) };
    // The target may be a heap `Value::Ref` OR — under the arena GC — a `Value::Obj`
    // (e.g. netty's AbstractChannelHandlerContext whose setAddComplete CASes its
    // `handlerState`). The old code only matched `Value::Ref`, so a CAS on an arena
    // object no-op'd to null → the bytecode's `ifeq` saw a non-int and underflowed.
    let target = match args.get(1) {
        Some(t @ (Value::Ref(_) | Value::Obj(_))) => t.clone(),
        _ => return Ok(Some(Some(Value::Null))),
    };
    // Resolve the declaring-class storage key from the receiver's own class —
    // the same identity bytecode getfield/putfield uses for this field.
    //
    // ⭐**THIS IS A CAS LOOP'S INNER TURN.** jctools' MPSC `offer()` spins
    // `compareAndSet(producerIndex, ...)` until it wins, and netty CASes handler state the same
    // way. `value_class_name` built a fresh `String` of the receiver's class name on every one
    // of those turns, purely to hand it to `resolve_field_id`, which hashes it and drops it.
    // `target_field_id` reads the name where it already lives.
    let fid = target_field_id(vm, &target, &field);
    let cur = vm.read_value_field(&target, fid).unwrap_or(Value::Null);
    let set = |vm: &mut Vm, v: Value| { vm.write_value_field(&target, fid, v); };
    let ret = match name {
        "get" => cur,
        "set" | "lazySet" => { set(vm, args.get(2).cloned().unwrap_or(Value::Null)); return Ok(Some(None)); }
        "getAndSet" => { set(vm, args.get(2).cloned().unwrap_or(Value::Null)); cur }
        "compareAndSet" | "weakCompareAndSet" => {
            let exp = args.get(2).cloned().unwrap_or(Value::Null);
            let new = args.get(3).cloned().unwrap_or(Value::Null);
            // An uninitialised numeric field reads as Null but represents 0 — a
            // long/int CAS against expected 0 must succeed (else jctools' MPSC
            // offer() CAS loop spins forever on its zero-valued producerIndex).
            let zero_exp = matches!(exp, Value::Long(0) | Value::Int(0));
            let hit = val_eq(&cur, &exp) || (matches!(cur, Value::Null) && zero_exp);
            if hit { set(vm, new); Value::Int(1) } else { Value::Int(0) }
        }
        "incrementAndGet" => { let v = Value::Int(cur.as_i32() + 1); set(vm, v.clone()); v }
        "decrementAndGet" => { let v = Value::Int(cur.as_i32() - 1); set(vm, v.clone()); v }
        "getAndIncrement" => { set(vm, Value::Int(cur.as_i32() + 1)); cur }
        "getAndDecrement" => { set(vm, Value::Int(cur.as_i32() - 1)); cur }
        "addAndGet" => { let v = Value::Int(cur.as_i32() + args.get(2).map(Value::as_i32).unwrap_or(0)); set(vm, v.clone()); v }
        "getAndAdd" => { set(vm, Value::Int(cur.as_i32() + args.get(2).map(Value::as_i32).unwrap_or(0))); cur }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

// --- atomic arrays ----------------------------------------------------------

/// MethodHandle.invoke/invokeExact — resolve the bound method and call it.
pub(crate) fn methodhandle_obj(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let mh = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    // One word for the two `resolve_member` sites below (constructor handle and ordinary
    // target); neither reads a gate of its own any more.
    let g = refl_gates();
    match name {
        "invoke" | "invokeExact" | "invokeWithArguments" => {
            let owner = field_rstr(vm, mh, "$class").unwrap_or_else(empty_rstr);
            let mname = field_rstr(vm, mh, "$mname").unwrap_or_else(empty_rstr);
            let mtype = field_rstr(vm, mh, "$mtype").unwrap_or_else(empty_rstr);
            let arg_desc = mtype_args(&mtype);
            let mut call: Vec<Value> = args[1..].to_vec();
            if name == "invokeWithArguments" {
                // single Object[]/List argument is spread.
                call = crate::natives::varargs(vm, args.get(1));
            }
            // Constructor handle: allocate then run <init>.
            if &*mname == "<init>" {
                let obj = vm.heap.alloc(&owner);
                let mut locals = vec![Value::Ref(obj)];
                locals.extend(call);
                if let Some(f) = resolve_member(vm, g, &owner, "<init>", &arg_desc)? {
                    vm.invoke(f.decl, "<init>", &f.desc, locals)?;
                }
                return Ok(Some(Some(Value::Ref(obj))));
            }
            match resolve_member(vm, g, &owner, &mname, &arg_desc)? {
                Some(f) => {
                    // TRAMPOLINE: this is a TAIL-position call (nothing after the
                    // invoke) — stage it as pending so the callee (e.g. the server's
                    // REAL main, reached via the bundler's MethodHandle) runs on the
                    // EXPLICIT frame stack (checkpointable). The pending machinery
                    // lands the return value on the caller frame's operand stack
                    // exactly as the direct return would.
                    if vm.trampoline
                        && f.decl.file.method(&mname, &f.desc).and_then(|m| m.code.as_ref()).is_some()
                    {
                        vm.pending = Some(crate::vmstate::PendingCall {
                            decl: f.decl,
                            name: std::sync::Arc::from(&*mname),
                            // The memo already holds the descriptor as an `Arc<str>`, so staging
                            // a pending call is now a refcount bump where it used to allocate
                            // and copy the descriptor.
                            desc: f.desc,
                            args: call,
                            // Reflection reaches the method by name; there is no call site to
                            // have cached its index.
                            midx: None,
                        });
                        return Ok(Some(None));
                    }
                    Ok(Some(vm.invoke(f.decl, &mname, &f.desc, call)?))
                }
                None => {
                    // No declared BYTECODE for the target — fall through to the NATIVE layer
                    // exactly as invokevirtual/invokestatic do (bytecode → native resolution
                    // order). A native-only stub reached through a MethodHandle (e.g.
                    // RuntimeMXBean.getInputArguments, which netty's PlatformDependent.
                    // estimateMaxDirectMemory invokes reflectively) must still answer; returning
                    // Null here made that path NPE on List.size. `call` already carries the
                    // receiver first for a virtual target, matching try_invoke's convention.
                    if let Some(r) = crate::natives::try_invoke(vm, &owner, &mname, &mtype, &call)? {
                        return Ok(Some(r));
                    }
                    Ok(Some(Some(Value::Null)))
                }
            }
        }
        "asType" | "bindTo" | "asFixedArity" | "asVarargsCollector" | "withVarargs" => Ok(Some(Some(Value::Ref(mh)))),
        _ => Ok(None),
    }
}

pub(crate) fn varhandle(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let vh = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    let Some(field) = field_rstr(vm, vh, "$field") else { return Ok(Some(Some(Value::Null))) };
    let tdesc = field_rstr(vm, vh, "$tdesc").unwrap_or_else(|| lit_rstr!("L"));
    let static_owner = field_rstr(vm, vh, "$static");
    // Instance access must use the same declaring-class storage key as bytecode
    // getfield/putfield — a bare-name read would miss (or shadow) the real slot.
    //
    // A STATIC handle addresses the class's `statics` map by NAME and never touches an instance
    // slot, so probing `$owner` and resolving a declaring-class field id for it was work whose
    // result no branch could read. VarHandle ops sit inside netty/jctools spin loops, so this is
    // a by-name heap probe plus a two-string hash removed from every turn of one.
    let fid = if static_owner.is_some() {
        0
    } else {
        let owner = field_rstr(vm, vh, "$owner").unwrap_or_else(empty_rstr);
        vm.resolve_field_id(&owner, &field)
    };

    // Read/write the target field — an instance (args[1]=receiver) or a static.
    let read = |vm: &mut Vm| -> Result<Value, VmError> {
        if let Some(owner) = &static_owner {
            if let Ok(c) = vm.loader.get_or_load(owner) { vm.ensure_init(&c)?;
                return Ok(vh_default(c.statics.read().unwrap_or_else(|e| e.into_inner()).get(&*field).cloned().unwrap_or(Value::Null), &tdesc)); }
            Ok(Value::Null)
        } else if let Some(Value::Ref(o)) = args.get(1) {
            Ok(vh_default(vm.heap.get_field_hint(*o, fid, 0).0, &tdesc))
        } else { Ok(Value::Null) }
    };
    let write = |vm: &mut Vm, val: Value| {
        if let Some(owner) = &static_owner {
            if let Ok(c) = vm.loader.get_or_load(owner) { let _ = vm.ensure_init(&c); vm.bump_static_gen(); c.statics.write().unwrap_or_else(|e| e.into_inner()).insert(field.to_string(), val); }
        } else if let Some(Value::Ref(o)) = args.get(1) {
            vm.heap.put_field_hint(*o, fid, val, 0);
        }
    };
    // The value/expected/new args sit after the receiver for instance handles.
    let vbase = if static_owner.is_some() { 1 } else { 2 };

    let n = name;
    let ret = if n.starts_with("get") && !n.starts_with("getAnd") && n != "getAndSet" && n != "getAndAdd" {
        read(vm)? // get / getVolatile / getAcquire / getOpaque
    } else if n == "set" || n.starts_with("setVolatile") || n.starts_with("setRelease") || n.starts_with("setOpaque") || n == "setPlain" {
        let v = args.get(vbase).cloned().unwrap_or(Value::Null);
        write(vm, v);
        return Ok(Some(None));
    } else if n.starts_with("compareAndSet") || n.starts_with("weakCompareAndSet") {
        let expected = args.get(vbase).cloned().unwrap_or(Value::Null);
        let newv = args.get(vbase + 1).cloned().unwrap_or(Value::Null);
        let cur = read(vm)?;
        if val_eq(&cur, &expected) { write(vm, newv); Value::Int(1) } else { Value::Int(0) }
    } else if n.starts_with("compareAndExchange") {
        let expected = args.get(vbase).cloned().unwrap_or(Value::Null);
        let newv = args.get(vbase + 1).cloned().unwrap_or(Value::Null);
        let cur = read(vm)?;
        if val_eq(&cur, &expected) { write(vm, newv); }
        cur
    } else if n.starts_with("getAndSet") {
        let newv = args.get(vbase).cloned().unwrap_or(Value::Null);
        let cur = read(vm)?;
        write(vm, newv);
        cur
    } else if n.starts_with("getAndAdd") {
        let add = args.get(vbase).and_then(Value::as_long).unwrap_or(0);
        let cur = read(vm)?;
        let sum = cur.as_long().unwrap_or(0) + add;
        write(vm, if matches!(tdesc.bytes().next(), Some(b'J')) { Value::Long(sum) } else { Value::Int(sum as i32) });
        cur
    } else {
        return Ok(None);
    };
    Ok(Some(Some(ret)))
}

// --- java.util.concurrent.TimeUnit ------------------------------------------

/// Parse ONE type signature at `b[i]`; returns the reflected Type value (a
/// Class mirror or a synthesized ParameterizedType) and the index just past it.
pub(crate) fn sig_type(vm: &mut Vm, b: &[u8], i: usize) -> Option<(Value, usize)> {
    match *b.get(i)? {
        b'L' => {
            let mut j = i + 1;
            let mut name = String::new();
            let mut args: Vec<Value> = Vec::new();
            loop {
                match *b.get(j)? {
                    b'<' => {
                        j += 1;
                        while *b.get(j)? != b'>' {
                            match *b.get(j)? {
                                b'*' => {
                                    args.push(vm.make_class("java/lang/Object"));
                                    j += 1;
                                }
                                b'+' | b'-' => {
                                    let (t, nj) = sig_type(vm, b, j + 1)?;
                                    args.push(t);
                                    j = nj;
                                }
                                _ => {
                                    let (t, nj) = sig_type(vm, b, j)?;
                                    args.push(t);
                                    j = nj;
                                }
                            }
                        }
                        j += 1; // past '>'
                    }
                    b';' => {
                        j += 1;
                        break;
                    }
                    b'.' => {
                        // Inner-class continuation: Outer<..>.Inner — flatten to $.
                        name.push('$');
                        j += 1;
                    }
                    c => {
                        name.push(c as char);
                        j += 1;
                    }
                }
            }
            let t = if args.is_empty() {
                vm.make_class(&name)
            } else {
                make_parameterized_type(vm, &name, args)
            };
            Some((t, j))
        }
        // Type variable T<name>; — erased to Object (we don't model bounds).
        b'T' => {
            let mut j = i + 1;
            while *b.get(j)? != b';' {
                j += 1;
            }
            Some((vm.make_class("java/lang/Object"), j + 1))
        }
        b'[' => {
            let (inner, j) = sig_type(vm, b, i + 1)?;
            // Array-of-T mirror by name: "[<inner raw name>".
            let inner_name = field_str(vm, Some(&inner), "$name").unwrap_or_default();
            Some((vm.make_class(&format!("[{inner_name}")), j))
        }
        c @ (b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z') => {
            let prim = match c {
                b'B' => "byte", b'C' => "char", b'D' => "double", b'F' => "float",
                b'I' => "int", b'J' => "long", b'S' => "short", _ => "boolean",
            };
            Some((vm.make_class(prim), i + 1))
        }
        _ => None,
    }
}
