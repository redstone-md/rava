//! Record shapes: the synthetic methods and the switch labels that match them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// Render a `print`/`println` argument per its declared type: `Z`→boolean,
/// `C`→char, everything else via the value's natural display.
pub(crate) fn format_print_arg(desc: &str, value: Option<&Value>) -> String {
    let arg_type = desc.as_bytes().get(1).copied();
    match (arg_type, value) {
        (Some(b'Z'), Some(v)) => if v.as_int().unwrap_or(0) != 0 { "true" } else { "false" }.to_string(),
        (Some(b'C'), Some(v)) => {
            char::from_u32(v.as_int().unwrap_or(0) as u32).map(String::from).unwrap_or_default()
        }
        (_, Some(v)) => v.display(),
        (_, None) => String::new(),
    }
}

/// Simple name of a record class, as the synthesised `toString` prints it.
///
/// `ObjectMethods.bootstrap` renders `getSimpleName() + "[" + ...`, and
/// `getSimpleName()` of a NESTED class is the part after the last `$`, not after
/// the last `/`. Splitting only on `/` printed a record declared inside a class
/// as `P3$Point[x=1, y=2]` where HotSpot prints `Point[x=1, y=2]` — the record's
/// whole guest-visible rendering, so anything that logs or string-keys a record
/// diverged. A LOCAL class is `Outer$1Name`, so the JVM also drops the leading
/// digits the compiler prepends for uniqueness.
pub(crate) fn record_simple_name(binary: &str) -> &str {
    let after_pkg = binary.rsplit('/').next().unwrap_or(binary);
    let after_outer = after_pkg.rsplit('$').next().unwrap_or(after_pkg);
    let stripped = after_outer.trim_start_matches(|c: char| c.is_ascii_digit());
    if stripped.is_empty() { "Record" } else { stripped }
}

/// Append one record component's `toString` rendering, per its DECLARED descriptor.
///
/// The component values are stored unboxed, so a `char` and a `boolean` both
/// arrive as `Value::Int` and the value's natural display printed `c=113`/`z=1`
/// where HotSpot prints `c=q`/`z=true`. Same rule as `format_print_arg` above,
/// keyed on a field descriptor rather than a method descriptor.
///
/// WRITTEN STRAIGHT INTO THE CALLER'S BUFFER for the two descriptor-decided arms. Both used to
/// return a fresh `String` — `"true".to_string()` of a `&'static str`, `String::from(char)` of a
/// single character — that the one caller immediately `push_str`ed into its own buffer and
/// dropped. That is one heap allocation per `boolean`/`char` component per record `toString`,
/// where the quantity that grows is components per record x record `toString` calls, and a record
/// `toString` is what renders every registry key the datapack reload string-keys. Byte-identical:
/// the same bytes those arms built, appended without the intermediate.
///
/// The remaining arm still goes through `value_to_string`, DELIBERATELY. Its render can re-enter
/// guest `toString`, and `value_to_string` owns the memo scope that bounds and invalidates that
/// recursion; skipping it for the primitive variants would save one more allocation apiece by
/// duplicating a contract that lives in `vm::strings`, and a stale memo entry is a wrong answer.
fn record_component_append(
    vm: &mut Vm,
    out: &mut String,
    v: &Value,
    desc: &str,
) -> Result<(), VmError> {
    match desc.as_bytes().first().copied() {
        Some(b'Z') => out.push_str(if v.as_int().unwrap_or(0) != 0 { "true" } else { "false" }),
        // `char::from_u32` refusing appended nothing before (`unwrap_or_default` of a `String`),
        // and appends nothing now.
        Some(b'C') => {
            if let Some(c) = char::from_u32(v.as_int().unwrap_or(0) as u32) {
                out.push(c);
            }
        }
        _ => out.push_str(&vm.value_to_string(v)?),
    }
    Ok(())
}

/// The link for this site + receiver, resolving it on first use. `None` whenever the
/// fast path does not apply (flag off, or a legacy heap-handle receiver that has no
/// cheap class id), and the caller then takes the historic per-call route.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn record_link(
    vm: &mut Vm,
    site: (usize, u16),
    recv: &Value,
    fields: &[String],
) -> Option<std::rc::Rc<RecordLink>> {
    if !record_link_enabled() {
        return None;
    }
    let Value::Obj(p) = recv else { return None };
    let cid = crate::nextgen::arena_class_id(*p);
    let key = (site.0, site.1, cid);
    if let Some(hit) = crate::opcodes::RECORD_LINKS.with(|c| c.borrow().get(&key).cloned()) {
        return Some(hit);
    }
    crate::residue_probe::record_link_built();
    let class = crate::nextgen::class_name(cid)?.to_string();
    let simple = record_simple_name(&class).to_string();
    let ids: Box<[u32]> = fields.iter().map(|f| vm.resolve_field_id(&class, f)).collect();
    let link = std::rc::Rc::new(RecordLink {
        class: class.into(),
        simple: simple.into(),
        ids,
    });
    crate::opcodes::RECORD_LINKS.with(|c| {
        c.borrow_mut().insert(key, std::rc::Rc::clone(&link));
    });
    Some(link)
}

#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn record_link(
    _vm: &mut Vm,
    _site: (usize, u16),
    _recv: &Value,
    _fields: &[String],
) -> Option<std::rc::Rc<RecordLink>> {
    None
}

/// `RAVA_NO_FAST_CODEC=1` restores the historic per-call derivations in
/// `record_object_method` (class-name `String`s built only to be compared, a
/// `Vec<String>` + `join` per `toString`, a rendered pair per component compare).
/// Default ON; also stands down with the block switch.
fn fast_codec_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FAST_CODEC").is_none())
        && !crate::flags::emit_more_disabled()
}

/// Do `a` and `b` have the SAME runtime class, WITHOUT rendering either name?
///
/// WAS: two `object_class_name` calls per synthesised `equals` — two fresh heap
/// copies of a ~50-character class name, for a value that is compared once and
/// dropped. That is O(len(class name)) bytes copied per record `equals`, and
/// record `equals` is called once per hash-map probe on a record key, so the
/// quantity that grows is *record-keyed lookups per boot* — the datapack reload's
/// registry maps are exactly that population. IS: one `u32` compare for two arena
/// receivers, one `&str` compare for two heap handles. No allocation either way.
///
/// SOUND, NOT OPTIMISTIC. `nextgen::register_class` dedups the class registry BY
/// NAME (it returns the index of the existing entry whenever the name is already
/// registered), so class id and class name are in bijection and equal ids ⇒ equal
/// names. The converse does NOT hold for the historic comparison: two UNREGISTERED
/// ids both render `None`, and `None == None` called them the same class — so the
/// unequal-id arm still compares the names, just as `Arc<str>` rather than as two
/// `String` copies. Mixed representations fall through to the historic route.
fn same_record_class(vm: &mut Vm, a: &Value, b: &Value) -> bool {
    if fast_codec_enabled() {
        #[cfg(feature = "next-gen-gc")]
        if let (Value::Obj(x), Value::Obj(y)) = (a, b) {
            let (ix, iy) =
                (crate::nextgen::arena_class_id(*x), crate::nextgen::arena_class_id(*y));
            return ix == iy || crate::nextgen::class_name(ix) == crate::nextgen::class_name(iy);
        }
        if let (Value::Ref(x), Value::Ref(y)) = (a, b) {
            // Two immutable borrows of the same heap — `class_of` hands out `&str`
            // INTO the heap, so the compare needs no copy at all.
            return vm.heap.class_of(*x) == vm.heap.class_of(*y);
        }
    }
    object_class_name(vm, a) == object_class_name(vm, b)
}

/// Component equality for the LEGACY (default) `equals` arm, when it can be
/// decided without rendering either side.
///
/// WAS: `value_to_string(a) != value_to_string(b)` for EVERY component — two
/// `String` allocations per component, so 2 x (components per record) heap
/// allocations per `equals`, to compare two `i32`s. IS: the compare itself for
/// the variants whose rendering is `Value::display` and injective.
///
/// IDENTICAL BY CONSTRUCTION, and only for those variants. `value_to_string`
/// routes `Int`/`Long`/`Str`/`Null` to `display()` on BOTH its fast and its memo
/// path: `i32::to_string`, `i64::to_string`, the string's own bytes, `"null"`.
/// Each is injective over its variant, so "renders equal" and "is equal" coincide,
/// and none of them runs guest code — no observable call is skipped.
///
/// DELIBERATELY EXCLUDED. `Float`/`Double`: `java_ftoa(-0.0) != java_ftoa(0.0)`
/// while `-0.0 == 0.0`, and two NaNs render identically while comparing unequal —
/// there the rendering is NOT the comparison, and the historic answer is the one
/// the guest has been observing. `Ref`/`Obj`: rendering them can invoke a guest
/// `toString`, which is an observable side effect that must still happen. Mixed
/// variants (`Null` against `Str("null")` renders EQUAL today) fall through.
fn fast_component_eq(a: &Value, b: &Value) -> Option<bool> {
    if !fast_codec_enabled() {
        return None;
    }
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x == y),
        (Value::Long(x), Value::Long(y)) => Some(x == y),
        (Value::Str(x), Value::Str(y)) => Some(x == y),
        (Value::Null, Value::Null) => Some(true),
        _ => None,
    }
}

/// Compute a record's synthesised equals/hashCode/toString from its fields.
/// Works for BOTH object representations: a legacy heap handle (`Value::Ref`)
/// and an arena-allocated record (`Value::Obj`). The old `Some(Value::Ref(h))`
/// receiver match dropped arena records to `Null`, which collapsed every record
/// to "null"/equal-alike — exactly what re-broke the ResourceKey$InternKey
/// interner under next-gen-gc (every registry key hashed/stringified identically
/// → "Adding duplicate key" at registry bootstrap).
/// `descs[i]` = `fields[i]`'s descriptor (see `Plan::Record`). Under
/// `RAVA_RECORD_SEMANTICS` the hashCode/equals arms use the real recursive
/// component semantics above; the legacy render arms remain the default AND the
/// depth-cap fallback.
pub(crate) fn record_object_method(
    vm: &mut Vm,
    site: (usize, u16),
    kind: &str,
    fields: &[String],
    descs: &[Box<str>],
    args: &[Value],
) -> Result<Value, VmError> {
    let recv = match args.first() {
        Some(v @ (Value::Ref(_) | Value::Obj(_))) => v.clone(),
        _ => return Ok(Value::Null),
    };
    // GC-SAFETY: recv (the record instance — an arena Value::Obj) + args are held in Rust
    // locals across value_to_string() below, which recursively builds Strings / invokes
    // guest toString → allocates → can trigger a moving collection. Without rooting, a
    // collection there evacuates recv young→old but this local keeps the old ptr → the NEXT
    // field read `object_field_by_id(&recv)` derefs a stale arena ptr → `PHASE5 arena getfield
    // class None' (RAVA_PROBE_STALE pinned: compiled DensityFunctions$Ap2.toString reading
    // Ap2#minValue on a stale receiver). Root them for the whole computation.
    // Rooting a CLONE in gc_protect keeps the object ALIVE across value_to_string()'s
    // allocation, but does NOT update this local `recv` (a separate copy). So push recv (and,
    // for equals, other) into gc_protect and RE-READ them from their slots before each field
    // access — the moving collector forwards gc_protect entries in place, so the re-read yields
    // the current pointer. A Drop guard truncates on every exit.
    #[cfg(feature = "next-gen-gc")]
    struct GpTrunc {
        _scope: crate::vm::thread_context::RootTrunc,
    }
    #[cfg(feature = "next-gen-gc")]
    let (rslot, _gp) = {
        let b = crate::vm::thread_context::root_len(&vm.gc_protect);
        crate::vm::thread_context::push_root(&mut vm.gc_protect, recv.clone());
        (
            b,
            GpTrunc {
                // SAFETY: the root vector's VM fallback and active context remain
                // valid until this per-call guard drops.
                _scope: unsafe {
                    crate::vm::thread_context::RootTrunc::new(&mut vm.gc_protect, b)
                },
            },
        )
    };
    // current (forwarded) receiver read.
    #[cfg(feature = "next-gen-gc")]
    macro_rules! recv_cur { () => { crate::vm::thread_context::root_index(&vm.gc_protect, rslot) }; }
    #[cfg(not(feature = "next-gen-gc"))]
    macro_rules! recv_cur { () => { recv.clone() }; }
    // Record fields live under their declaring-class storage key (the same
    // identity bytecode getfield uses) — a bare-name read would miss them.
    // LINKED PATH: `rcls` and every component id come from the site's link, so this
    // call allocates no class name and resolves no field by name (see `RecordLink`).
    let link = record_link(vm, site, &recv, fields);
    // UNLINKED PATH — and that is the DEFAULT path: `record_link_enabled()` reads
    // `RAVA_RECORD_LINK`, which is opt-in. So every synthesised toString/hashCode/
    // equals reached `object_class_name`, which hands back an owned `String`, i.e.
    // a fresh heap COPY of a ~50-character class name that is then only borrowed
    // as a `&str` (by `record_simple_name` and `resolve_field_id`) and dropped.
    // The class registry ALREADY owns that name as an `Arc<str>` keyed by the
    // receiver's class id, so clone the pointer and deref it.
    // Was O(len(class name)) bytes copied per record method call, is one refcount
    // bump. The quantity that grows is record method calls per boot.
    #[cfg(feature = "next-gen-gc")]
    let arena_cls: Option<std::sync::Arc<str>> = match &recv {
        Value::Obj(p) if link.is_none() && fast_codec_enabled() => {
            crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
        }
        _ => None,
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let arena_cls: Option<std::sync::Arc<str>> = None;
    let unlinked_cls: String;
    let rcls: &str = match (&link, &arena_cls) {
        (Some(l), _) => &l.class,
        (None, Some(a)) => &**a,
        (None, None) => {
            unlinked_cls = object_class_name(vm, &recv).unwrap_or_default();
            &unlinked_cls
        }
    };
    // Resolve one component's field id: an indexed read on the linked path, the
    // historic name-keyed lookup otherwise. Identical results by construction —
    // the link's ids are exactly `resolve_field_id(class, fields[i])`.
    macro_rules! field_id {
        ($i:expr, $f:expr) => {
            match link.as_ref().and_then(|l| l.ids.get($i).copied()) {
                Some(id) => id,
                None => vm.resolve_field_id(rcls, $f),
            }
        };
    }
    // ONE SWITCH READ FOR BOTH TALLIES. Each of these opens with its own `enabled()` — an
    // acquire-ordered `OnceLock` read that LLVM may not fold with its twin — and they are armed
    // and disarmed together by one env var. This runs once per synthesised record method, which
    // the `RecordLink` note above measures at ~90 k calls per `RsProbe` run and names as the
    // dominant guest work of `RandomState.create`.
    if crate::residue_probe::enabled() {
        crate::residue_probe::record_method(kind, rcls);
        crate::residue_probe::record_call(link.is_some(), fields.len());
    }
    match kind {
        "toString" => {
            let simple: &str = match &link {
                Some(l) => &l.simple,
                None => record_simple_name(rcls),
            };
            // ONE buffer, appended in place. WAS: a `format!` per component (one
            // `String` each), a `Vec` to hold them, a `join` buffer and a final
            // `format!` = N+3 allocations for a record with N components, where the
            // N intermediates exist only to be concatenated. IS: 1. Byte-identical
            // output — same separator, same order, same per-component rendering.
            // Grows with components per record x record `toString` calls, and a
            // record `toString` is what renders every registry key the reload
            // string-keys.
            let mut out = String::with_capacity(simple.len() + 2 + fields.len() * 24);
            out.push_str(simple);
            out.push('[');
            for (i, f) in fields.iter().enumerate() {
                let id = field_id!(i, f);
                let rc = recv_cur!();
                let v = object_field_by_id(vm, &rc, id);
                // Rendered by the component's DECLARED descriptor: char and boolean
                // are stored as ints and would otherwise print as their codes.
                let desc = descs.get(i).map(|d| &**d).unwrap_or("");
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(f);
                out.push('=');
                record_component_append(vm, &mut out, &v, desc)?;
            }
            out.push(']');
            Ok(Value::Str(out.into()))
        }
        "hashCode" => {
            // JVM ObjectMethods semantics (RAVA_RECORD_SEMANTICS, default OFF):
            // 31-fold `Objects.hashCode` over the COMPONENT VALUES. The depth
            // ticket refuses past the cap → legacy render-hash serves the subtree.
            if crate::record_semantics_enabled() {
                if let Some(_depth) = RecordSemDepth::enter() {
                    let mut h = 0i32;
                    for (i, f) in fields.iter().enumerate() {
                        let id = field_id!(i, f);
                        let rc = recv_cur!();
                        let v = object_field_by_id(vm, &rc, id);
                        let desc = descs.get(i).map(|d| &**d).unwrap_or("");
                        let fh = record_component_hash(vm, &v, desc)?;
                        h = h.wrapping_mul(31).wrapping_add(fh);
                    }
                    return Ok(Value::Int(h));
                }
            }
            let mut h = 0i32;
            for (i, f) in fields.iter().enumerate() {
                let id = field_id!(i, f);
                let rc = recv_cur!();
                let fh = match object_field_by_id(vm, &rc, id) {
                    Value::Int(i) => i,
                    r @ (Value::Ref(_) | Value::Obj(_)) => {
                        let s = vm.value_to_string(&r)?;
                        s.chars().fold(0i32, |a, c| a.wrapping_mul(31).wrapping_add(c as i32))
                    }
                    Value::Long(l) => (l ^ (l >> 32)) as i32,
                    Value::Str(s) => s.chars().fold(0i32, |a, c| a.wrapping_mul(31).wrapping_add(c as i32)),
                    _ => 0,
                };
                h = h.wrapping_mul(31).wrapping_add(fh);
            }
            Ok(Value::Int(h))
        }
        "equals" => {
            let other = match args.get(1) {
                Some(v @ (Value::Ref(_) | Value::Obj(_))) => v.clone(),
                _ => return Ok(Value::Int(0)),
            };
            #[cfg(feature = "next-gen-gc")]
            let oslot = {
                let b = crate::vm::thread_context::root_len(&vm.gc_protect);
                crate::vm::thread_context::push_root(&mut vm.gc_protect, other.clone());
                b
            };
            #[cfg(feature = "next-gen-gc")]
            macro_rules! other_cur { () => { crate::vm::thread_context::root_index(&vm.gc_protect, oslot) }; }
            #[cfg(not(feature = "next-gen-gc"))]
            macro_rules! other_cur { () => { other.clone() }; }
            {
                let rc = recv_cur!();
                let oc = other_cur!();
                if !same_record_class(vm, &rc, &oc) {
                    return Ok(Value::Int(0));
                }
            }
            // JVM ObjectMethods semantics (RAVA_RECORD_SEMANTICS, default OFF):
            // identity shortcut, then recursive component comparison. The depth
            // ticket refuses past the cap → legacy render-compare serves it.
            if crate::record_semantics_enabled() {
                if let Some(_depth) = RecordSemDepth::enter() {
                    // `this == o` shortcut of the synthesised equals.
                    let same = match (&recv_cur!(), &other_cur!()) {
                        (Value::Ref(x), Value::Ref(y)) => x == y,
                        #[cfg(feature = "next-gen-gc")]
                        (Value::Obj(x), Value::Obj(y)) => x.addr() == y.addr(),
                        _ => false,
                    };
                    if same {
                        return Ok(Value::Int(1));
                    }
                    for (i, f) in fields.iter().enumerate() {
                        let id = field_id!(i, f);
                        let rc = recv_cur!();
                        let a = object_field_by_id(vm, &rc, id);
                        let oc = other_cur!();
                        let b = object_field_by_id(vm, &oc, id);
                        if !record_component_eq(vm, &a, &b)? {
                            return Ok(Value::Int(0));
                        }
                    }
                    return Ok(Value::Int(1));
                }
            }
            for (i, f) in fields.iter().enumerate() {
                let id = field_id!(i, f);
                let rc = recv_cur!();
                let a = object_field_by_id(vm, &rc, id);
                let oc = other_cur!();
                let b = object_field_by_id(vm, &oc, id);
                // Decide it without rendering when rendering IS the compare (see
                // `fast_component_eq`); otherwise the historic render-and-compare.
                if let Some(eq) = fast_component_eq(&a, &b) {
                    if !eq {
                        return Ok(Value::Int(0));
                    }
                    continue;
                }
                if vm.value_to_string(&a)? != vm.value_to_string(&b)? {
                    return Ok(Value::Int(0));
                }
            }
            Ok(Value::Int(1))
        }
        _ => Ok(Value::Null),
    }
}

/// Does `target` match a pattern-switch `label`? Class label → instanceof;
/// String label → enum-constant/String-name equality; Integer label → int eq.
pub(crate) fn switch_label_matches(vm: &mut Vm, target: &Value, label: &Value) -> bool {
    match label {
        Value::Ref(h) if vm.heap.class_of(*h) == Some("java/lang/Class") => {
            match vm.heap.get_field_syn(*h, crate::heap::Syn::Name) {
                Value::Str(name) => instance_of(vm, target, &name),
                _ => false,
            }
        }
        // ⭐**THE NAME IS COMPARED, NEVER KEPT — SO IT IS NEVER COPIED.** Every arm below handed
        // back `n.to_string()`: a fresh heap copy of an enum-constant or `String` name, built to
        // be `==`'d against the label on the very next line and dropped. `indy`'s switch
        // dispatcher calls this ONCE PER LABEL until one matches, so a `switch` over a K-constant
        // enum copied up to K names per execution — O(sum of label-name lengths) bytes per switch,
        // where what grows is switch executions x labels per switch, and Java 21 sealed/record
        // codecs are one long chain of these. `RStr` derefs to `str` and `Option::as_deref` is
        // already how the compare is written, so keeping the borrow costs one `Arc` refcount at
        // most and the comparison is unchanged.
        Value::Str(s) => {
            let tname = match target {
                Value::Ref(t) => match vm.heap.get_field(*t, "$enumName") {
                    Value::Str(n) => Some(n),
                    _ => match vm.heap.get_field_syn(*t, crate::heap::Syn::Name) { Value::Str(n) => Some(n), _ => None },
                },
                // ENUM MIGRATION (RAVA_ARENA_ENUM): an arena-born enum carries its
                // name in the injected `$enumName` layout cell. Read it via arena
                // getfield when the slot exists; flag OFF never lands here (enums
                // stay handles → the Value::Ref arm above).
                #[cfg(feature = "next-gen-gc")]
                Value::Obj(p) if crate::nextgen::arena_enum_enabled() => {
                    // Read: the L1, not the interner. See `Vm::synth_get`. This arm yields an
                    // `Option`, so an un-interned name folds into the same `None` the `slot_of`
                    // miss below produces.
                    let cid = crate::nextgen::arena_class_id(*p);
                    if let Some(id) = vm
                        .heap
                        .field_id_opt("$enumName")
                        .filter(|&id| crate::nextgen::slot_of(cid, id).is_some())
                    {
                        match arena_getfield(*p, id) {
                            Value::Str(n) => Some(n),
                            _ => None,
                        }
                    } else {
                        None
                    }
                }
                // `target` is borrowed, so this one clone is an `Arc` refcount bump — not the
                // byte copy `to_string` was.
                Value::Str(ts) => Some(ts.clone()),
                _ => None,
            };
            tname.as_deref() == Some(s.as_ref())
        }
        Value::Int(n) => target.as_int() == Some(*n),
        _ => false,
    }
}

/// First descriptor byte of each parameter type (arrays collapse to '['), used
/// to stringify StringConcatFactory args by type (Z→true/false, C→char).
pub(crate) fn concat_param_types(desc: &str) -> Vec<u8> {
    let bytes = desc.as_bytes();
    let mut i = desc.find('(').map(|p| p + 1).unwrap_or(0);
    let mut out = Vec::new();
    while i < bytes.len() && bytes[i] != b')' {
        let is_array = bytes[i] == b'[';
        while i < bytes.len() && bytes[i] == b'[' { i += 1; } // collapse array dims
        out.push(if is_array { b'[' } else { bytes[i] });
        match bytes[i] {
            b'L' => { while i < bytes.len() && bytes[i] != b';' { i += 1; } i += 1; }
            _ => i += 1,
        }
    }
    out
}
