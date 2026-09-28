//! Function combinators, enum sets, service loaders and method handles.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

pub(crate) fn fn_combinator(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    match name {
        "identity" => {
            let h = vm.heap.alloc("java/util/function/Function");
            vm.heap.put_field_syn(h, crate::heap::Syn::FnKind, Value::Str("identity".into()));
            Ok(Some(Some(Value::Ref(h))))
        }
        "andThen" | "compose" => {
            let h = vm.heap.alloc("java/util/function/Function");
            vm.heap.put_field_syn(h, crate::heap::Syn::FnKind, Value::Str(name.into()));
            if let Some(first) = args.first().cloned() { vm.heap.put_field(h, "$first", first); }
            if let Some(second) = args.get(1).cloned() { vm.heap.put_field(h, "$second", second); }
            Ok(Some(Some(Value::Ref(h))))
        }
        "apply" => {
            // apply on a synthetic Function object -> Vm::apply.
            let f = args.first().cloned().unwrap_or(Value::Null);
            Ok(Some(vm.apply(&f, vec![args.get(1).cloned().unwrap_or(Value::Null)])?))
        }
        _ => Ok(None),
    }
}

// --- java.util.Comparator ---------------------------------------------------

/// An enum constant's `ordinal()`, or `i32::MAX` for anything that is not one — so a
/// stray value sorts last instead of pretending to be first.
fn ordinal_of(vm: &mut Vm, v: Option<&Value>) -> i32 {
    // Through `enum_field`, NOT `heap.get_field`: a guest enum's constants are ARENA
    // objects under next-gen-gc, and a `Value::Ref`-only read reported `MAX` for every
    // one of them — so the first version of this sorted nothing at all and looked like
    // it worked, because a stable sort leaves an all-equal input alone.
    crate::natives::enum_field(vm, v, "$enumOrd")
        .and_then(|x| x.as_int())
        .unwrap_or(i32::MAX)
}

/// Order enum constants the way every `EnumSet` and `EnumMap` view must: by ordinal.
fn sort_by_ordinal(vm: &mut Vm, elems: &mut [Value]) {
    let mut keyed: Vec<(i32, Value)> =
        elems.iter().map(|v| (ordinal_of(vm, Some(v)), v.clone())).collect();
    keyed.sort_by_key(|(o, _)| *o);
    for (slot, (_, v)) in elems.iter_mut().zip(keyed.into_iter()) {
        *slot = v;
    }
}

/// Every constant of the enum class that `sample` belongs to, in declaration order —
/// obtained the same way `allOf` does, by invoking the synthetic static `values()`.
fn enum_constants_of(vm: &mut Vm, sample: Option<&Value>) -> Result<Vec<Value>, VmError> {
    // The declaring class, from EITHER representation — a guest enum's constants are
    // arena objects, and a `Value::Ref`-only lookup returned no class, so `range` and
    // `complementOf` produced empty sets rather than failing loudly.
    let cls = match sample {
        Some(Value::Ref(h)) => match vm.heap.class_of(*h) {
            Some(c) => c.to_string(),
            None => return Ok(Vec::new()),
        },
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) => match crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)) {
            Some(c) => c.to_string(),
            None => return Ok(Vec::new()),
        },
        _ => return Ok(Vec::new()),
    };
    if let Ok(c) = vm.loader.get_or_load(&cls) {
        vm.ensure_init(&c)?;
    }
    Ok(match vm.resolve_by_args(&cls, "values", "")? {
        Some((decl, desc, _)) => match vm.invoke(decl, "values", &desc, vec![])? {
            Some(Value::Array(a)) => vm.heap.array_values(a).unwrap_or_default(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    })
}

pub(crate) fn enum_set(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let to_list = |vm: &mut Vm, elems: Vec<Value>| {
        let arr = vm.heap.alloc_values("Ljava/lang/Object;", elems);
        let h = vm.heap.alloc("java/util/ArrayList");
        vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
        Value::Ref(h)
    };
    match name {
        "allOf" => {
            // Invoke the enum class's synthetic static values() to list constants.
            let cls = field_str(vm, args.first(), "$name").unwrap_or_default();
            if let Ok(c) = vm.loader.get_or_load(&cls) {
                vm.ensure_init(&c)?;
            }
            let elems = match vm.resolve_by_args(&cls, "values", "")? {
                Some((decl, desc, _)) => match vm.invoke(decl, "values", &desc, vec![])? {
                    Some(Value::Array(h)) => vm.heap.array_values(h).unwrap_or_default(),
                    _ => Vec::new(),
                },
                None => Vec::new(),
            };
            Ok(Some(Some(to_list(vm, elems))))
        }
        "noneOf" => Ok(Some(Some(to_list(vm, Vec::new())))),
        // AN ENUM SET ITERATES IN ORDINAL ORDER, always — that is its whole contract and
        // the reason it exists rather than a `HashSet`. `of` kept ARGUMENT order, so
        // `EnumSet.of(WEST, NORTH)` printed `[WEST, NORTH]` where the JVM prints
        // `[NORTH, WEST]`, and anything iterating one saw the constants out of order.
        "of" => {
            let mut e = args.to_vec();
            sort_by_ordinal(vm, &mut e);
            Ok(Some(Some(to_list(vm, e))))
        }
        // `range(from, to)` — every constant of the declaring class whose ordinal lies
        // between the two, inclusive. Absent entirely (`UnsatisfiedLinkError`).
        "range" => {
            let lo = ordinal_of(vm, args.first());
            let hi = ordinal_of(vm, args.get(1));
            let all = enum_constants_of(vm, args.first())?;
            let e = all
                .into_iter()
                .filter(|c| {
                    let o = ordinal_of(vm, Some(c));
                    o >= lo && o <= hi
                })
                .collect();
            Ok(Some(Some(to_list(vm, e))))
        }
        // `complementOf(set)` — every constant of the same class NOT in the argument.
        "complementOf" => {
            let present: Vec<i32> = match args.first() {
                Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Elems) {
                    // Spare capacity is not elements — see `Heap::elems_values`.
                    Value::Array(a) => vm
                        .heap
                        .elems_values(*h, a)
                        .iter()
                        .map(|v| ordinal_of(vm, Some(v)))
                        .collect(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            let sample = match args.first() {
                Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Elems) {
                    Value::Array(a) => vm.heap.elems_values(*h, a).first().cloned(),
                    _ => None,
                },
                _ => None,
            };
            let all = enum_constants_of(vm, sample.as_ref())?;
            let e = all
                .into_iter()
                .filter(|c| !present.contains(&ordinal_of(vm, Some(c))))
                .collect();
            Ok(Some(Some(to_list(vm, e))))
        }
        "copyOf" => {
            // copyOf(Collection) — reuse the source's elements.
            let elems = match args.first() {
                Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Elems) {
                    Value::Array(a) => vm.heap.elems_values(*h, a),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            Ok(Some(Some(to_list(vm, elems))))
        }
        _ => Ok(None),
    }
}

// --- java.util.ServiceLoader ------------------------------------------------

pub(crate) fn service_loader(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    match name {
        "load" => {
            // load(Class) | load(Class, ClassLoader) — args[0] is the service Class.
            let iface = field_str(vm, args.first(), "$name").unwrap_or_default();
            let h = vm.heap.alloc("java/util/ServiceLoader");
            vm.heap.put_field(h, "$iface", Value::Str(iface.into()));
            Ok(Some(Some(Value::Ref(h))))
        }
        "iterator" => {
            let iface = field_str(vm, args.first(), "$iface").unwrap_or_default();
            let mut instances = Vec::new();
            for cls in service_impls(vm, &iface) {
                instances.push(new_instance(vm, &cls, "", &[])?);
            }
            Ok(Some(Some(crate::stream::iterator_of(vm, instances))))
        }
        "stream" => {
            let iface = field_str(vm, args.first(), "$iface").unwrap_or_default();
            let providers: Vec<Value> = service_impls(vm, &iface)
                .into_iter()
                .map(|cls| {
                    let p = vm.heap.alloc("java/util/ServiceLoader$Provider");
                    vm.heap.put_field(p, "$cls", Value::Str(cls.into()));
                    Value::Ref(p)
                })
                .collect();
            Ok(Some(Some(crate::stream::of_values(vm, providers))))
        }
        "forEach" => {
            let iface = field_str(vm, args.first(), "$iface").unwrap_or_default();
            for cls in service_impls(vm, &iface) {
                let inst = new_instance(vm, &cls, "", &[])?;
                // Re-read the Consumer per iteration (args is rooted in place by
                // try_invoke's NativeArgsGuard) — a pre-loop clone would go stale if
                // an arena-Obj lambda moved during an earlier apply/new_instance.
                let f = args.get(1).cloned().unwrap_or(Value::Null);
                vm.apply(&f, vec![inst])?;
            }
            Ok(Some(None))
        }
        "reload" => Ok(Some(None)),
        _ => Ok(None),
    }
}

pub(crate) fn methodhandles(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    match name {
        "lookup" | "publicLookup" | "privateLookupIn" => {
            Ok(Some(Some(Value::Ref(vm.heap.alloc("java/lang/invoke/MethodHandles$Lookup")))))
        }
        "findVarHandle" | "findStaticVarHandle" => {
            // (Class owner, String name, Class type)
            let fname = match args.get(2) { Some(Value::Str(s)) => s.to_string(), _ => String::new() };
            let tdesc = field_str(vm, args.get(3), "$name").map(|n| desc_token(&n)).unwrap_or_else(|| "L".into());
            let vh = vm.heap.alloc("java/lang/invoke/VarHandle");
            vm.heap.put_field(vh, "$field", Value::Str(fname.into()));
            vm.heap.put_field(vh, "$tdesc", Value::Str(tdesc.into()));
            // Owner class: instance handles need it to resolve the field's
            // declaring-class storage key (same identity as bytecode getfield).
            let owner = field_str(vm, args.get(1), "$name").unwrap_or_default();
            vm.heap.put_field(vh, "$owner", Value::Str(owner.clone().into()));
            if name == "findStaticVarHandle" {
                vm.heap.put_field(vh, "$static", Value::Str(owner.into()));
            }
            Ok(Some(Some(Value::Ref(vh))))
        }
        // findStatic/findVirtual/findSpecial(Class, name, MethodType) -> MethodHandle
        "findStatic" | "findVirtual" | "findSpecial" | "findConstructor" | "unreflect" => {
            let mh = vm.heap.alloc("java/lang/invoke/MethodHandle");
            let owner = field_str(vm, args.get(1), "$name").unwrap_or_default();
            // findConstructor(Class, MethodType): no name arg; others: (Class, name, MethodType).
            let (mname, mtype_idx) = if name == "findConstructor" {
                ("<init>".to_string(), 2)
            } else {
                (match args.get(2) { Some(Value::Str(s)) => s.to_string(), _ => "<init>".to_string() }, 3)
            };
            let mtype = field_str(vm, args.get(mtype_idx), "$desc").unwrap_or_default();
            vm.heap.put_field(mh, "$class", Value::Str(owner.into()));
            vm.heap.put_field(mh, "$mname", Value::Str(mname.into()));
            vm.heap.put_field(mh, "$mtype", Value::Str(mtype.into()));
            vm.heap.put_field(mh, "$virtual", Value::Int((name == "findVirtual" || name == "findSpecial") as i32));
            Ok(Some(Some(Value::Ref(mh))))
        }
        "in" | "lookupClass" => Ok(Some(Some(args.first().cloned().unwrap_or(Value::Null)))),
        _ => Ok(None),
    }
}

pub(crate) fn methodtype(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    match name {
        "methodType" | "genericMethodType" => {
            let rdesc = field_str(vm, args.first(), "$name").map(|n| desc_token(&n)).unwrap_or_else(|| "V".into());
            let mut params = String::new();
            for a in &args[1..] {
                match a {
                    Value::Array(h) => {
                        for c in vm.heap.array_values(*h).unwrap_or_default() {
                            if let Value::Ref(ch) = c {
                                if let Value::Str(n) = vm.heap.get_field_syn(ch, crate::heap::Syn::Name) { params.push_str(&desc_token(&n)); }
                            }
                        }
                    }
                    Value::Ref(ch) => {
                        if let Value::Str(n) = vm.heap.get_field_syn(*ch, crate::heap::Syn::Name) { params.push_str(&desc_token(&n)); }
                    }
                    _ => {}
                }
            }
            let desc = format!("({params}){rdesc}");
            let h = vm.heap.alloc("java/lang/invoke/MethodType");
            vm.heap.put_field(h, "$desc", Value::Str(desc.into()));
            Ok(Some(Some(Value::Ref(h))))
        }
        "toMethodDescriptorString" | "toString" => Ok(Some(Some(Value::Str(field_str(vm, args.first(), "$desc").unwrap_or_default().into())))),
        "parameterCount" => {
            let d = field_str(vm, args.first(), "$desc").unwrap_or_default();
            let n = crate::descriptor::arg_count(&d);
            Ok(Some(Some(Value::Int(n as i32))))
        }
        "returnType" | "changeReturnType" | "wrap" | "unwrap" | "erase" | "generic" => {
            Ok(Some(Some(args.first().cloned().unwrap_or(Value::Null))))
        }
        _ => Ok(None),
    }
}

pub(crate) fn timeunit_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let unit = field_str(vm, args.first(), "$name").unwrap_or_default();
    // Nanoseconds-per-unit.
    let nanos_per: i64 = match unit.as_str() {
        "NANOSECONDS" => 1,
        "MICROSECONDS" => 1_000,
        "MILLISECONDS" => 1_000_000,
        "SECONDS" => 1_000_000_000,
        "MINUTES" => 60_000_000_000,
        "HOURS" => 3_600_000_000_000,
        "DAYS" => 86_400_000_000_000,
        _ => 1_000_000,
    };
    let d = args.get(1).map(Value::as_i64).unwrap_or(0);
    let total_nanos = d.wrapping_mul(nanos_per);
    let ret = match name {
        "toNanos" => Value::Long(total_nanos),
        "toMicros" => Value::Long(total_nanos / 1_000),
        "toMillis" => Value::Long(total_nanos / 1_000_000),
        "toSeconds" => Value::Long(total_nanos / 1_000_000_000),
        "toMinutes" => Value::Long(total_nanos / 60_000_000_000),
        "toHours" => Value::Long(total_nanos / 3_600_000_000_000),
        "toDays" => Value::Long(total_nanos / 86_400_000_000_000),
        "sleep" => return Ok(Some(None)),
        "name" | "toString" => Value::Str(unit.into()),
        "convert" => Value::Long(d), // best-effort identity
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

// --- java.lang.management MXBeans (canned values) ---------------------------

pub(crate) fn mxbean_method(vm: &mut Vm, name: &str, _args: &[Value]) -> Handled {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let empty_list = |vm: &mut Vm| -> Value {
        let arr = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
        let l = vm.heap.alloc("java/util/ArrayList");
        vm.heap.put_field_syn(l, crate::heap::Syn::Elems, Value::Array(arr));
        Value::Ref(l)
    };
    let ret = match name {
        "getStartTime" => Value::Long(now - 1000),
        "getUptime" => Value::Long(1000),
        "getName" => Value::Str("1@rava".into()),
        "getVmName" => Value::Str("Rava".into()),
        "getVmVendor" => Value::Str("Rava Project".into()),
        "getVmVersion" | "getSpecVersion" => Value::Str("22".into()),
        "getInputArguments" | "getGarbageCollectorMXBeans" => empty_list(vm),
        "getAvailableProcessors" => Value::Int(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as i32),
        "getArch" => Value::Str(std::env::consts::ARCH.into()),
        "getVersion" => Value::Str("10.0".into()),
        "getSystemLoadAverage" => Value::Double(-1.0),
        "getTotalPhysicalMemorySize" | "getTotalMemorySize" | "getCommittedVirtualMemorySize" => Value::Long(8 * 1024 * 1024 * 1024),
        "getFreePhysicalMemorySize" | "getFreeMemorySize" => Value::Long(4 * 1024 * 1024 * 1024),
        "getHeapMemoryUsage" | "getNonHeapMemoryUsage" => Value::Ref(vm.heap.alloc("java/lang/management/MemoryUsage")),
        "getUsed" | "getCommitted" | "getInit" => Value::Long(256 * 1024 * 1024),
        "getMax" => Value::Long(2 * 1024 * 1024 * 1024),
        "getObjectName" => Value::Null,
        "isThreadCpuTimeSupported" | "isThreadContentionMonitoringSupported" => Value::Int(0),
        _ => Value::Null,
    };
    Ok(Some(Some(ret)))
}

// --- java.lang.Runtime ------------------------------------------------------

pub(crate) fn stackwalker(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    match name {
        "getInstance" => Ok(Some(Some(Value::Ref(vm.heap.alloc("java/lang/StackWalker"))))),
        // walk(fn): no real frames modelled → hand the function an empty stream.
        "walk" => {
            let f = args.get(1).cloned().unwrap_or(Value::Null);
            let empty = crate::stream::of_values(vm, Vec::new());
            Ok(Some(vm.apply(&f, vec![empty])?))
        }
        "getCallerClass" => {
            // The caller two frames up (skip getCallerClass's own caller).
            let caller = crate::vm::thread_context::with_call_stack(
                &vm.call_stack,
                |call_stack| call_stack.iter().rev().nth(1).map(|(c, _)| c.clone()),
            )
            .unwrap_or_else(|| "java/lang/Object".to_string());
            Ok(Some(Some(vm.make_class(&caller))))
        }
        "forEach" => Ok(Some(None)),
        _ => Ok(None),
    }
}

// --- java.lang.ThreadLocal (single-threaded: one slot) ----------------------
