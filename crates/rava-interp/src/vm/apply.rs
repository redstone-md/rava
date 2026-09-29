//! Applying a resolved method, including SAM shapes and class initialisation.

use crate::*;

/// The declaring class, SAM name, full descriptor and METHOD INDEX a `(class, arity)` pair
/// resolves to.
///
/// ⚠**THE DESCRIPTOR IS AN `Arc<str>`, NOT A `Box<str>`, AND THE READ IS WHY.** A memo hit has
/// to copy the entry OUT of the `RefCell` before it can call `&mut self`, and a `Box<str>` has
/// no share — `.cloned()` was a malloc and a memcpy of the descriptor bytes, per apply, for a
/// string this table already owns and never mutates. An `Arc` clones by refcount.
///     was: one allocation the length of the descriptor per SAM apply; is: none.
///
/// ⚠**AND IT CARRIES THE INDEX.** `Vm::invoke` re-finds the method BY NAME on every call —
/// hashing the name and the descriptor and memcmp-ing both against the overload set — to reach
/// a `MemberInfo` this memo identified once. `invoke_idx` takes the index instead, which is the
/// same shape a cached interpreted call site already uses (`SiteRef::midx`).
///     was: one `ClassFile::method` hash + compare per SAM apply; is: an array index.
/// Sound for the same reason the rest of the entry is: a loaded class's method table is
/// immutable and classes are never unloaded, so the index names the same method forever.
type SamHit = (Arc<LoadedClass>, &'static str, Arc<str>, u32);

thread_local! {
    /// ⭐**THE SAM OF A CLASS IS A CONSTANT OF THE CLASS, AND `apply_sam` RE-DERIVED IT PER CALL.**
    ///
    /// For arity 1 the candidate list is five names and each one ran `resolve_named`, which walks
    /// up the super chain allocating a `String` per level (`start.to_string()`, then
    /// `super_class.clone()`), calls `get_or_load` per level (owner-name hash + an `Arc` refcount
    /// pair out of the SHARED class cache) and then LINEARLY scans that level's method table
    /// comparing name, `code.is_some()` and `count_args(descriptor)`.
    ///
    ///     was:  O(candidate names x superclasses in the chain x methods declared per class),
    ///           plus one malloc per (name, level), EVERY apply
    ///     is:   one hash probe, and the walk only on the first apply of a (class, arity)
    ///
    /// Every term of that walk is fixed by the receiver's class the moment it is loaded: classes
    /// are never unloaded, a loaded class's method table is immutable, and the walk returns at the
    /// FIRST level that declares a body — so nothing below the winner can change the answer later.
    ///
    /// Keyed by `instanceof_class_key`: the interned class-NAME pointer for a handle, the arena
    /// class id for a pointer object. Same identity and same stability argument as `IOF_MEMO` —
    /// neither is an object address or a handle, so a swept-and-reused handle presents its NEW
    /// class and simply misses. `None` (a lambda, whose functional interface is a property of the
    /// OBJECT and not of its class; a primitive) means no memo, and the walk runs as it always did.
    ///
    /// SIZE: 48 bytes an entry, and the population is the number of distinct (class, arity) pairs
    /// ever SAM-applied — hundreds on a boot, low tens of KB. Unlike a direct-mapped table it is
    /// probed by one hash and never scanned, so its cost does not grow with its size.
    ///
    /// ⚠**ONLY HITS ARE STORED.** A miss falls through to the native-dispatch tail, whose answer
    /// depends on loader state that can still change (a class not loadable now may be synthesised
    /// later) — writing that off permanently is exactly the negative-cache bug `alloc.rs` records.
    static SAM_MEMO: std::cell::RefCell<crate::FastMap<(u64, u32), SamHit>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// **DEFAULT-ON**, `RAVA_NO_FAST_APPLY=1` disables, and so does `RAVA_NO_EMIT_MORE` so the whole
/// round bisects in one run.
///
/// One switch for the three places on the host->guest apply path where something is BUILT per
/// apply that the apply did not need built. They are switched together, and deliberately: each
/// removes allocations from a different caller of the same path, and a per-site switch would
/// report three separate zeros and miss their sum.
///
///   * `Vm::sam_locals` — the invocation vector. `vec![recv]` allocates at capacity ONE, so the
///     `extend` that follows always reallocates: two mallocs and a free per apply for a vector
///     whose exact length is known before the first `Value` moves.
///   * the `$fnkind == "collect"` consumer in [`Vm::apply`] — a QUADRATIC append; see there.
///   * `Vm::drive_green`'s frame resume — the body found by NAME on every return, exactly the
///     defect `drive_stack` already fixed by reading the pointer `Frame::new` was handed.
///
/// Off, each site runs the code it replaced, byte for byte. Nothing else in this round is behind
/// it because nothing else in it has a behaviour to switch: replacing `start.to_string()` with a
/// borrow of the name already inside the `LoadedClass`, or a fresh `Arc<str>` copy of a class
/// name with a clone of the interned one, produces the identical bytes by construction — the
/// same reason `ensure_init`'s operand commute carries no switch either.
pub(crate) fn fast_apply() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_APPLY").is_none() && !crate::flags::emit_more_disabled()
    })
}

impl Vm {
    pub(crate) fn apply(
        &mut self,
        func: &Value,
        args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        match func {
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) if crate::nextgen::is_arena_lambda(*p) => {
                if crate::comparator_plan::cmp_plan_census_enabled() {
                    let n = crate::comparator_plan::AL_GENERAL_APPLY
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    // WHO routes 2 M arena-lambda calls here? The counters proved the direct arm
                    // loses 4.13 M hits with every miss reason flat, so these calls are diverted
                    // BEFORE the direct probe — and no counter can say by whom. The compiled
                    // caller stack can, in one run. First few only; this is the hot path.
                    // The guest stack came back EMPTY for every sample, which is not an
                    // unarmed instrument — `jit_method_stack` is ungated and reads the TLS
                    // compiled frames push, so empty means the caller is a NATIVE (or the
                    // interpreter), not compiled code. A guest stack cannot name a Rust
                    // caller; a Rust backtrace can, and 5 samples of it cost nothing.
                    if n < 5 {
                        eprintln!(
                            "[AL-CALLER] #{n} guest={:?}\n{}",
                            crate::jit::jit_method_stack(6),
                            std::backtrace::Backtrace::force_capture()
                        );
                    }
                }
                self.invoke_lambda_value(func, args)
            }
            // EXISTENCE, not a snapshot: `lambda_of(..).is_some()` cloned the whole
            // `Lambda` — a `Vec<Value>` allocation — and dropped it again, on every
            // apply, purely to answer a bool that `is_lambda` answers from the slot.
            Value::Ref(h) if self.heap.is_lambda(*h) => self.invoke_lambda(*h, args),
            Value::Ref(h) => {
                // Synthetic functional objects (Function.identity, composed fns).
                match self.heap.get_field_syn(*h, crate::heap::Syn::FnKind) {
                    Value::Str(k) => match k.as_ref() {
                        "identity" => Ok(args.into_iter().next()),
                        // Collecting consumer used to drain a Spliterator.
                        "collect" => {
                            if let Value::Ref(sink) = self.heap.get_field(*h, "$sink") {
                                let elem = args.into_iter().next().unwrap_or(Value::Null);
                                let cur = self.heap.get_field_syn(sink, crate::heap::Syn::Elems);
                                // ⭐**A QUADRATIC APPEND: n COLLECTED ELEMENTS REBUILT THE SINK
                                // ARRAY n TIMES.**
                                //
                                // This consumer is the sink that a generic
                                // `Spliterator.tryAdvance` drain and `mapMulti` push into, ONE
                                // element per apply — and every apply copied every element
                                // already collected OUT of the array, pushed one onto the copy,
                                // and allocated a WHOLE NEW array for the result, leaving the
                                // previous one for the GC. Over a drain of n elements (the
                                // pipeline's own guard bounds it at 5,000,000) that is n array
                                // allocations and n^2/2 `Value` copies to append n values.
                                //     was: O(elements already collected) copied and one guest
                                //          array allocated PER element appended;
                                //     is:  O(1) amortised, and no array allocated at all.
                                //
                                // `Heap::array_push` exists for exactly this shape — it was
                                // written for the same defect in the collection natives, whose
                                // note records the boot-time O(n^2) of building a guava list
                                // element by element. `$elems` arrays are ours and growable;
                                // they are not fixed-length Java arrays.
                                //
                                // ⚠**PRECONDITION, PROVED AND NOT ASSUMED.** `array_push`
                                // appends at the array's real end; the rebuild appended at
                                // `elems_len`, the LOGICAL length, which a guest class that
                                // overrides the collection may hold BELOW the capacity via
                                // `$size`. Where the two differ this path would write into
                                // spare capacity and lose the element, so it refuses and the
                                // rebuild below runs exactly as it always did. (A sink built
                                // here is native-fabricated, so they are equal — but the sink
                                // is read out of a guest field and this code does not get to
                                // assume who wrote it.)
                                let mut pending = Some(elem);
                                if let Value::Array(a) = &cur {
                                    let a = *a;
                                    if fast_apply()
                                        && self.heap.elems_len(sink, a)
                                            == self.heap.array_len(a).unwrap_or(usize::MAX)
                                    {
                                        if let Some(v) = pending.take() {
                                            self.heap.array_push(a, v);
                                        }
                                    }
                                }
                                if let Some(v) = pending {
                                    let mut e = match cur {
                                        // Spare capacity is not elements — see `Heap::elems_values`.
                                        Value::Array(a) => self.heap.elems_values(sink, a),
                                        _ => Vec::new(),
                                    };
                                    e.push(v);
                                    let arr = self.heap.alloc_values("Ljava/lang/Object;", e);
                                    self.heap.put_field_syn(sink, crate::heap::Syn::Elems, Value::Array(arr));
                                }
                            }
                            Ok(None)
                        }
                        "andThen" | "compose" => {
                            let (first, second) = (
                                self.heap.get_field(*h, "$first"),
                                self.heap.get_field(*h, "$second"),
                            );
                            let (f, s) = if k.as_ref() == "compose" {
                                (second, first)
                            } else {
                                (first, second)
                            };
                            let mid = self.apply(&f, args)?.unwrap_or(Value::Null);
                            self.apply(&s, vec![mid])
                        }
                        "negate" => {
                            let first = self.heap.get_field(*h, "$first");
                            let r = self
                                .apply(&first, args)?
                                .and_then(|v| v.as_int())
                                .unwrap_or(0);
                            Ok(Some(Value::Int((r == 0) as i32)))
                        }
                        "and" | "or" => {
                            let (first, second) = (
                                self.heap.get_field(*h, "$first"),
                                self.heap.get_field(*h, "$second"),
                            );
                            let a = self
                                .apply(&first, args.clone())?
                                .and_then(|v| v.as_int())
                                .unwrap_or(0)
                                != 0;
                            let r = if k.as_ref() == "and" {
                                a && self
                                    .apply(&second, args)?
                                    .and_then(|v| v.as_int())
                                    .unwrap_or(0)
                                    != 0
                            } else {
                                a || self
                                    .apply(&second, args)?
                                    .and_then(|v| v.as_int())
                                    .unwrap_or(0)
                                    != 0
                            };
                            Ok(Some(Value::Int(r as i32)))
                        }
                        _ => Err(VmError::UnsupportedNative(
                            "apply on unknown functional".into(),
                        )),
                    },
                    // Otherwise treat it as an object implementing a SAM: dispatch
                    // its single functional method by common name.
                    _ => self.apply_sam(Value::Ref(*h), args),
                }
            }
            // Arena object implementing a functional interface (lambda/SAM that
            // lives on the moving arena, not the heap). It carries no synthetic
            // heap fields, so route straight to the SAM dispatcher with the arena
            // value itself as `this`. No class hardcode — the dispatcher keys on
            // the class's own SAM method, resolved by name + arity.
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(_) => self.apply_sam(func.clone(), args),
            other => Err(VmError::UnsupportedNative(format!(
                "apply on non-functional {}",
                other.display()
            ))),
        }
    }

    /// Apply an object that implements a functional interface by invoking its
    /// SAM (tried by the usual functional method names). The receiver may be a
    /// heap `Ref` or an arena `Value::Obj`; it is passed through unchanged as the
    /// invocation's `this`, so the arena GC keeps tracking it across the call.
    pub(crate) fn sam_candidates(arity: usize) -> &'static [&'static str] {
        match arity {
            0 => &[
                "get",
                "call",
                "run",
                "getAsInt",
                "getAsLong",
                "getAsBoolean",
            ],
            1 => &["apply", "test", "accept", "applyAsInt", "applyAsLong"],
            _ => &["apply", "test", "accept", "compare"],
        }
    }

    /// The invocation locals for a SAM apply — receiver first, then the arguments — in ONE
    /// buffer taken from the per-thread pool.
    ///
    /// ⭐**`vec![recv]` ALLOCATES AT CAPACITY ONE, SO THE `extend` AFTER IT ALWAYS REALLOCATES.**
    /// Two mallocs, a memcpy of everything already written, and a free of the caller's argument
    /// vector — per apply, for a vector whose exact length (`1 + arity`) is known before the
    /// first `Value` moves. The interpreter already keeps a per-thread buffer pool for precisely
    /// this: `build_frame_idx` takes the frame's buffer out of it and hands the caller's vector
    /// back, and `invoke_idx` returns whatever it is given to the same pool. A SAM apply can
    /// therefore pass through the call path allocating nothing at all.
    ///
    ///     was: 2 allocations + 1 free per apply, the memcpy growing with the SAM's arity;
    ///     is:  0 — a pooled buffer out, the caller's buffer back in.
    ///
    /// The argument values are MOVED (`drain`), not copied, so the vector handed to the pool is
    /// empty: no `Value` outlives this call inside a pooled buffer, and the drop order of the
    /// arguments is what it was. `RAVA_NO_FAST_APPLY=1` restores the two-allocation shape.
    fn sam_locals(&mut self, recv: &Value, mut args: Vec<Value>) -> Vec<Value> {
        if !fast_apply() {
            let mut locals = vec![recv.clone()];
            locals.extend(args);
            return locals;
        }
        let mut locals = self.take_vec();
        locals.reserve(args.len() + 1);
        locals.push(recv.clone());
        locals.extend(args.drain(..));
        self.give_vec(args);
        locals
    }

    pub(crate) fn apply_sam(&mut self, recv: Value, args: Vec<Value>) -> Result<Option<Value>, VmError> {
        let arity = args.len();
        // The key is taken FIRST, before anything is derived: on a hit this whole body is one hash
        // probe, and even `value_class_name` — which builds a `String` per call for no reason but
        // to be hashed again by `resolve_named` — is never reached. See `SAM_MEMO`.
        let key = if crate::vm::fast_handoff() {
            crate::opcodes::instanceof_class_key(&self.heap, &recv)
        } else {
            None
        };
        if let Some(k) = key {
            if let Some((decl, m, desc, midx)) =
                SAM_MEMO.with(|c| c.borrow().get(&(k, arity as u32)).cloned())
            {
                let locals = self.sam_locals(&recv, args);
                // BY INDEX, not by name: see `SamHit`. `invoke` is `invoke_idx` with `None`.
                return self.invoke_idx(&decl, Some(midx), m, &desc, locals);
            }
        }
        let class = self.value_class_name(&recv);
        // Common SAM names by arity; resolve the first that exists with a body.
        let candidates = Self::sam_candidates(arity);
        for m in candidates {
            // resolve by arg count (descriptors vary); reuse resolve_by_args with
            // a wildcard by scanning the class for a matching-name method.
            if let Some((decl, desc, _)) = self.resolve_named(&class, m, arity)? {
                // Bind the site: which of the candidate names won, where its body lives, and
                // WHICH ENTRY of that class's method table it is, are now settled for this class
                // for the life of the process. The index is found by the one linear scan this
                // whole memo exists to stop repeating — `resolve_named` has just walked the same
                // table, so it costs the fill and nothing else. A method table that somehow does
                // not contain the member it just yielded simply is not memoised, and the walk
                // runs again next apply.
                if let Some(k) = key {
                    if let Some(midx) = decl
                        .file
                        .methods
                        .iter()
                        // The full predicate `resolve_named` selected on, so the index names
                        // the member it returned and not a same-named neighbour.
                        .position(|mm| mm.name == *m && mm.descriptor == desc && mm.code.is_some())
                    {
                        SAM_MEMO.with(|c| {
                            c.borrow_mut().insert(
                                (k, arity as u32),
                                (Arc::clone(&decl), *m, Arc::from(desc.as_str()), midx as u32),
                            );
                        });
                    }
                }
                let locals = self.sam_locals(&recv, args);
                return self.invoke(decl, m, &desc, locals);
            }
        }
        // Native-emulated functionals have NO bytecode SAM: Executors$RunnableAdapter
        // (Executors.callable), PrivilegedAction wrappers, guava task adapters, etc.
        // Route to the native dispatch, which keys on class+name and ignores the
        // descriptor — try each candidate name over the receiver's super chain. (A
        // FutureTask wrapping such an adapter reaches here via run() -> apply.)
        // BORROWED, NOT REBUILT. `super_chain` hands back a shared `Arc<[Box<str>]>` the loader
        // already owns and caches; this collected it into a fresh `Vec<String>` — one malloc per
        // ANCESTOR, per failed-SAM apply — to pass each name to a `&str` parameter. Holding the
        // `Arc` borrows nothing from `self`, so the `&mut self` calls below are unaffected.
        //     was: O(superclasses in the chain) allocations per apply; is: none.
        let chain = self.super_chain(&class);
        let locals = self.sam_locals(&recv, args);
        let desc = match arity {
            0 => "()Ljava/lang/Object;",
            1 => "(Ljava/lang/Object;)Ljava/lang/Object;",
            _ => "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        };
        for m in candidates {
            for cn in chain.iter() {
                if let Some(ret) = crate::natives::try_invoke(self, cn, m, desc, &locals)? {
                    return Ok(ret);
                }
            }
        }
        if std::env::var_os("RAVA_TRACE_NOSAM").is_some() {
            let extra = match &recv {
                #[cfg(feature = "next-gen-gc")]
                Value::Obj(p) => format!("Obj addr={:#x} arena_lambda={} cid={}", p.addr(), crate::nextgen::is_arena_lambda(*p), crate::nextgen::arena_class_id(*p)),
                Value::Ref(h) => format!("Ref({h}) is_lambda={}", self.heap.is_lambda(*h)),
                o => format!("{o:?}"),
            };
            eprintln!("[NOSAM] class='{class}' arity={arity} recv={extra}");
            eprintln!("[NOSAM-BT]\n{}", std::backtrace::Backtrace::force_capture());
        }
        Err(VmError::UnsupportedNative(format!(
            "apply on {class} (no SAM)"
        )))
    }

    /// Find a method by name + argument count (any descriptor) in the hierarchy.
    pub(crate) fn resolve_named(
        &mut self,
        start: &str,
        name: &str,
        argc: usize,
    ) -> Result<Option<(Arc<LoadedClass>, String, bool)>, VmError> {
        // ⭐**WALKED BY BORROW, NOT BY COPY.** The chain used to be driven by an OWNED class
        // name: `start.to_string()` to enter it and `super_class.clone()` to advance it. Both
        // are `String`s built only to be handed straight back as a `&str` to `get_or_load`, and
        // the name they copy already lives inside the `LoadedClass` this loop holds an `Arc` to
        // — which cannot be freed while the loop is looking at it.
        //     was: O(superclasses in the chain) String allocations and frees per resolve;
        //     is:  none.
        // Byte-identical results, so no switch: the loop visits the same classes in the same
        // order and stops on the same conditions.
        let mut cur = self.loader.get_or_load(start);
        loop {
            let class = match cur {
                Ok(c) => c,
                // A worker refusal is not "this class has no such method": stopping the walk here
                // would return Ok(None), and the caller reads that as a resolution FAILURE rather
                // than as "ask again serially".
                #[cfg(feature = "next-gen-gc")]
                Err(LoadError::WorkerRefused(_)) => return Err(VmError::RegionAbort),
                Err(_) => break,
            };
            if let Some(m) =
                class.file.methods.iter().find(|m| {
                    m.name == name && m.code.is_some() && count_args(&m.descriptor) == argc
                })
            {
                let is_static = m.access_flags & 0x0008 != 0;
                return Ok(Some((Arc::clone(&class), m.descriptor.clone(), is_static)));
            }
            // The borrow of `class` ends with the call it feeds, so the next level can be
            // assigned before `class` is dropped at the end of this iteration.
            cur = match class.file.super_class.as_deref() {
                Some(sup) => self.loader.get_or_load(sup),
                None => break,
            };
        }
        Ok(None)
    }

    /// `ensure_init` for a call site that takes it on an ACCESS, not on an initialisation.
    ///
    /// `getstatic_resolve` and the `new` path called `ensure_init` unconditionally: an out-of-line
    /// call that, for a class which finished initialising long ago, still performs a relaxed load
    /// of `initialized`, a SeqCst probe of the process-wide thread counter, a load of
    /// `clinit_done` and a load of `init_failed` — four loads and a call to re-derive an answer
    /// that latched permanently the first time it was asked. `ensure_init_fast` decides exactly
    /// that case inline in one acquire load and leaves every other §5.5 state — never started, in
    /// progress on this thread, in progress on another, erroneous — to the real `ensure_init`,
    /// untouched.
    ///
    /// Gated so the whole round bisects on one variable; `RAVA_NO_FAST_HANDOFF=1` (or
    /// `RAVA_NO_FAST_CLINIT=1`, or `RAVA_NO_EMIT_MORE=1`) restores the plain call at every site.
    #[inline(always)]
    pub(crate) fn ensure_init_handoff(&mut self, class: &Arc<LoadedClass>) -> Result<(), VmError> {
        if crate::vm::fast_handoff() {
            return self.ensure_init_fast(class);
        }
        self.ensure_init(class)
    }

    /// Run `<clinit>` once, initialising the superclass first (JVMS 5.5).
    pub(crate) fn ensure_init(&mut self, class: &Arc<LoadedClass>) -> Result<(), VmError> {
        if class.initialized.load(std::sync::atomic::Ordering::Relaxed) {
            // ⭐**JVMS §5.5 STEP 2: ANOTHER THREAD IS STILL RUNNING `<clinit>` → WAIT FOR IT.**
            //
            // `initialized` latches when `<clinit>` STARTS, to break init cycles — correct, and
            // for one thread indistinguishable from "finished". For two it is a hole with no
            // floor: the second thread reads this latch, concludes the class is ready, and runs
            // on statics the owner has not assigned yet. That is what the offloaded datapack
            // reload hit — a `Codec` field still null/half-built when the worker read it, so
            // `Codec$RecursiveCodec.decode` dispatched on a `DataResult$Error`.
            //
            // The JVM's rule is exactly the distinction this code was missing: the INITIALIZING
            // THREAD may re-enter (that is the cycle break), every other thread BLOCKS. Nothing
            // below runs while single-threaded, so the default path is the same latch it was.
            //
            // ⭐**THE CHEAP TEST GOES FIRST, AND IT IS THE ONE THAT SETTLES THE CASE.** These two
            // operands were the other way round, so every caller — including a class whose
            // `<clinit>` completed twenty seconds ago — paid `has_real_threads()`, a **SeqCst**
            // load of a process-wide counter, before the relaxed load that would have said "there
            // is nothing to wait for". SeqCst is the expensive member and not for its `mov`: it is
            // a barrier LLVM may not hoist out of a loop or fold across, in a body inlined
            // nowhere. `&&` is short-circuit and both operands are pure loads with no side
            // effects, so commuting them is bit-identical for every state — it changes only WHICH
            // load a finished class pays. (Hence no kill switch on this one: there is no new
            // behaviour to switch off.)
            if !class.clinit_done.load(std::sync::atomic::Ordering::Relaxed)
                && crate::gil::has_real_threads()
            {
                let me = crate::monitor::this_thread_id();
                // Bounded: a `<clinit>` that never completes must not hang the engine — the
                // pre-existing behaviour (proceed on partial statics) is bad, but a wedged VM
                // is worse, and this arc has already produced five hangs.
                for _ in 0..2000 {
                    let owner = class.init_owner.load(std::sync::atomic::Ordering::Acquire);
                    if owner == 0 || owner == me {
                        break; // ours to recurse into, or finished/never claimed
                    }
                    if class.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    crate::monitor::park_without_gil(1);
                }
            }
            // ERRONEOUS state (JVM §5.5): a class whose <clinit> threw must throw
            // NoClassDefFoundError on every later initialization attempt — NOT
            // silently proceed on half-initialized statics.
            if class.init_failed.load(std::sync::atomic::Ordering::Relaxed) {
                let msg = format!("Could not initialize class {}", class.file.this_class);
                return Err(VmError::Throw(self.make_exception("java/lang/NoClassDefFoundError", &msg)));
            }
            return Ok(());
        }
        // G1.4 CLINIT BARRIER: a GIL-free fork worker must NEVER initialise a class — running <clinit>
        // executes guest bytecode that mutates SHARED statics, and flipping `initialized`/`clinit_done`
        // + caching the layout are shared-metadata writes that race main. A worker touching an
        // as-yet-uninitialised class (e.g. an enum reached via initCache→getCollisionShape whose
        // <clinit> hasn't run on main) is the corruption hazard behind the netty enum crash. Abort the
        // region so the serial retry runs the init on main. (initialized is NOT set → main inits later.)
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::gilfree() {
            // The serial retry initializes classes in guest execution order, after
            // the coordinator has retired the aborted region's reservations.
            crate::parallel::request_abort();
            // Name it under the CHEAP gate as well. `fork_trace` also arms per-call tallies heavy
            // enough that a boot under it dies before reaching the ring, so requiring it to learn
            // WHICH class means never learning it for the phase that matters.
            if crate::parallel::fork_trace() || crate::parallel::lane_abort_site_enabled() {
                eprintln!(
                    "[CLINIT-BARRIER] worker hit UNINIT class {} (has_clinit={}) → region abort → serial retry",
                    class.file.this_class,
                    class.file.method("<clinit>", "()V").is_some()
                );
                // WHO reached it. The class name alone says a worker touched cold metadata; the
                // guest frames say WHY, and for `net/minecraft/util/ThreadingDetector` — MC's own
                // one-thread-at-a-time guard — the difference between "cold class" and "the guest
                // just detected our concurrency" is the whole diagnosis.
                let stack = crate::jit::jit_method_stack(8);
                if !stack.is_empty() {
                    eprintln!("[CLINIT-BARRIER]   guest frames: {}", stack.join(" <- "));
                }
            }
            return Ok(()); // leave uninitialised; the serial path initialises it on main
        }
        #[cfg(feature = "clinit-census")]
        let _clinit_census = crate::vm::clinit_census::Scope::begin(class);
        class.initialized.store(true, std::sync::atomic::Ordering::Relaxed); // set first to break init cycles
        // Claim the initialization for THIS thread (JVMS §5.5). Read by the wait above; the
        // release store below publishes every static this `<clinit>` writes.
        class.init_owner.store(
            crate::monitor::this_thread_id(),
            std::sync::atomic::Ordering::Release,
        );

        if let Some(sup) = class.file.super_class.clone() {
            if let Ok(s) = self.loader.get_or_load(&sup) {
                self.ensure_init(&s)?;
            }
        }
        if class.file.method("<clinit>", "()V").is_some() {
            // RAVA_TRACE_CLINIT — surface a <clinit> that throws. Because `initialized`
            // is set true BEFORE the body runs (cycle-break, above), a throw here leaves
            // the class half-initialised: its later statics stay at default (null/0)
            // forever, and if some caller swallows the error the corruption is silent
            // (e.g. netty DefaultChannelId.MACHINE_ID → null → arraylength NPE).
            let r = self.invoke(Arc::clone(class), "<clinit>", "()V", vec![]);
            if let Err(ref e) = r {
                class.init_failed.store(true, std::sync::atomic::Ordering::Relaxed); // erroneous — later attempts throw NCDFE
                if std::env::var_os("RAVA_TRACE_CLINIT").is_some() {
                    eprintln!("[CLINIT-THROW] {} <clinit> aborted: {:?}", class.file.this_class, e);
                }
                // Drop the §5.5 lock on the erroneous path too, or every other thread waits out
                // the full bound for a class that will never complete.
                class.init_owner.store(0, std::sync::atomic::Ordering::Release);
            }
            r?;
        }
        // The clinit body (if any) has now fully completed synchronously.
        class.clinit_done.store(true, std::sync::atomic::Ordering::Release);
        class.init_owner.store(0, std::sync::atomic::Ordering::Release); // release the §5.5 lock
        // Cache this class's field layout at init — BEFORE any instance can exist
        // — so the alloc choke-point pre-populates EVERY object of it (the `new`
        // path AND native heap.alloc), making the const-slot getfield's slot valid
        // without the id verify (true universal pre-population).
        if layout_alloc_enabled() {
            let name = class.file.this_class.clone();
            let _ = self.class_field_layout(&name);
        }
        Ok(())
    }

    /// (next-gen-gc) Initialise `class` by PUSHING its `<clinit>` frames onto the
    /// explicit `frames` stack instead of running them on the Rust stack — so a
    /// clinit's own allocations are trampolined and can hit the loop safepoint GC
    /// (Phase 3c). Push order: this class's clinit first (bottom), then the super
    /// chain (supers end up on top → driven first), so supers init before this
    /// clinit body. Cycle-safe (the `initialized` flag is set before recursing).
    /// UNUSED since the 3c-parity fix (clinits now init synchronously — the deferred
    /// frame left an initialized-latched window interleaved reads could observe);
    /// kept as the starting point for CHECKPOINTABLE clinits (needs a window-free
    /// design: latch only on frame completion + cycle detection via a running-set).
    #[allow(dead_code)]
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn ensure_init_tramp(
        &mut self,
        class: &Arc<LoadedClass>,
        frames: &mut Vec<(Frame, Arc<str>, Arc<str>)>,
    ) -> Result<(), VmError> {
        if class.initialized.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        class.initialized.store(true, std::sync::atomic::Ordering::Relaxed);
        // The field layout is independent of <clinit> — cache it now so the
        // retried `new` finds it ready.
        if layout_alloc_enabled() {
            let name = class.file.this_class.clone();
            let _ = self.class_field_layout(&name);
        }
        if class.file.method("<clinit>", "()V").is_some() {
            let f = self.build_frame(class, "<clinit>", "()V", vec![])?;
            frames.push((f, Arc::from("<clinit>"), Arc::from("()V")));
            // clinit_done stays FALSE — the body runs later, when the driver reaches
            // this pushed frame. drive_stack sets it on the frame's Normal return.
        } else {
            // No clinit body to defer → already "done".
            class.clinit_done.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(sup) = class.file.super_class.clone() {
            if let Ok(s) = self.loader.get_or_load(&sup) {
                self.ensure_init_tramp(&s, frames)?;
            }
        }
        Ok(())
    }

    /// Find the class in `start`'s hierarchy that declares `name+desc` with a
    /// body. Stops (returns None) when a superclass isn't loadable — that's the
    /// JDK boundary where a native must take over.
    pub(crate) fn resolve_method(
        &self,
        start: &str,
        name: &str,
        descriptor: &str,
    ) -> Result<Option<Arc<LoadedClass>>, VmError> {
        let dry_run = crate::ClassLoader::dry_run_loads_active();
        // Cache lookup via a reused thread-local scratch key so a HIT (the
        // overwhelmingly common case) allocates nothing. The cache is per-thread
        // (a pure function of immutable post-boot loader state).
        if !dry_run {
            if let Some(hit) = crate::tlcache::resolve_get(start, name, descriptor) {
                return Ok(hit);
            }
        }
        // 1. The class and its superclasses (these take priority).
        //
        // Walked by BORROW — see the note in `resolve_named`. A memo MISS is what reaches this
        // loop, and there is one per distinct (start, name, descriptor) triple the boot asks
        // for; each used to allocate and free one class name per level of the hierarchy purely
        // to pass it as a `&str`.
        //     was: O(superclasses in the chain) String allocations per resolve MISS; is: none.
        let mut cur = self.loader.get_or_load(start);
        let mut interfaces = Vec::new();
        let mut superclass_unloadable = false;
        loop {
            let class = match cur {
                Ok(c) => c,
                // A worker refusal is NOT "this superclass does not exist": treating it as one
                // would resolve the method against a truncated hierarchy and return a DIFFERENT
                // method than the serial path would. Abort the region instead.
                #[cfg(feature = "next-gen-gc")]
                Err(LoadError::WorkerRefused(_)) => return Err(VmError::RegionAbort),
                Err(LoadError::NotFound(_)) => {
                    superclass_unloadable = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            if class
                .file
                .method(name, descriptor)
                .map(|m| m.code.is_some())
                .unwrap_or(false)
            {
                if !dry_run {
                    crate::tlcache::resolve_put(start, name, descriptor, Some(Arc::clone(&class)));
                }
                return Ok(Some(class));
            }
            interfaces.extend(class.file.interfaces.iter().cloned());
            cur = match class.file.super_class.as_deref() {
                Some(sup) => self.loader.get_or_load(sup),
                None => break,
            };
        }
        // 2. Default methods from implemented interfaces — THE MOST SPECIFIC ONE.
        //
        // ⛔THIS USED TO RETURN THE FIRST ONE FOUND, which is a different method. With
        //
        //     interface A { default String who() { return "A"; } }
        //     interface B extends A { default String who() { return "B"; } }
        //     interface C extends A { }
        //     class D implements B, C { }
        //
        // the walk popped `C`, found nothing, pushed its parent `A`, and returned `A` —
        // where Java answers `B`. Not a missing method: a WRONG one, silently, on the
        // mechanism this jar builds its codecs and registries out of.
        //
        // The rule is the language's: a default method is inherited only if no
        // SUBINTERFACE of its declaring interface also declares one. So collect every
        // candidate and drop each one that some other candidate extends.
        let mut seen = std::collections::HashSet::new();
        let mut candidates: Vec<Arc<LoadedClass>> = Vec::new();
        while let Some(iname) = interfaces.pop() {
            if !seen.insert(iname.clone()) {
                continue;
            }
            if let Ok(i) = self.loader.get_or_load(&iname) {
                if i.file
                    .method(name, descriptor)
                    .map(|m| m.code.is_some())
                    .unwrap_or(false)
                {
                    candidates.push(Arc::clone(&i));
                }
                // Keep walking even after a hit: a MORE specific declaration can sit on a
                // branch not yet visited, and stopping here is exactly the old bug.
                interfaces.extend(i.file.interfaces.iter().cloned());
            }
        }
        if !candidates.is_empty() {
            let names: Vec<String> =
                candidates.iter().map(|c| c.file.this_class.to_string()).collect();
            // Keep a candidate only when no OTHER candidate extends it.
            let winner = candidates
                .iter()
                .enumerate()
                .find(|(k, c)| {
                    let me = c.file.this_class.to_string();
                    !names.iter().enumerate().any(|(j, other)| {
                        j != *k && self.interface_extends(other, &me)
                    })
                })
                .map(|(_, c)| Arc::clone(c))
                // A genuine ambiguity (two unrelated interfaces both declaring it) is an
                // `IncompatibleClassChangeError` in javac's output, so no compiled jar
                // reaches here with one; pick deterministically rather than fail.
                .unwrap_or_else(|| Arc::clone(&candidates[0]));
            if !dry_run {
                crate::tlcache::resolve_put(start, name, descriptor, Some(Arc::clone(&winner)));
            }
            return Ok(Some(winner));
        }
        let _ = superclass_unloadable;
        if !dry_run {
            crate::tlcache::resolve_put(start, name, descriptor, None);
        }
        Ok(None)
    }

    /// Does interface `sub` extend `sup`, transitively? The specificity test for default
    /// methods: among two candidates, the one the other extends is the LESS specific.
    fn interface_extends(&self, sub: &str, sup: &str) -> bool {
        if sub == sup {
            return false;
        }
        let mut stack = vec![sub.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            let Ok(c) = self.loader.get_or_load(&n) else { continue };
            for parent in c.file.interfaces.iter() {
                if parent.as_str() == sup {
                    return true;
                }
                stack.push(parent.to_string());
            }
        }
        false
    }

    /// Class name + its superclass chain (names readable even when a super
    /// isn't loadable, since each class file records its super's name). Used to
    /// find the JDK ancestor that declares a native (e.g. Enum.ordinal).
    pub(crate) fn super_chain(&self, start: &str) -> std::sync::Arc<[Box<str>]> {
        self.loader.super_chain(start)
    }

    /// Resolve `name` in `start`'s hierarchy by argument descriptor only
    /// (reflection has param types but not the return type). Returns the
    /// declaring class, the full descriptor, and whether the method is static.
    pub(crate) fn resolve_by_args(
        &mut self,
        start: &str,
        name: &str,
        arg_desc: &str,
    ) -> Result<Option<(Arc<LoadedClass>, String, bool)>, VmError> {
        // Walked by BORROW — see the note in `resolve_named`. The stream pipeline resolves
        // `tryAdvance` through here per generic spliterator drain and reflection resolves every
        // member miss through it, each paying one class-name String per level of the hierarchy.
        //     was: O(superclasses in the chain) String allocations per resolve; is: none.
        let mut cur = self.loader.get_or_load(start);
        loop {
            let class = match cur {
                Ok(c) => c,
                #[cfg(feature = "next-gen-gc")]
                Err(LoadError::WorkerRefused(_)) => return Err(VmError::RegionAbort),
                Err(LoadError::NotFound(_)) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            if let Some(m) =
                class.file.methods.iter().find(|m| {
                    m.name == name && m.code.is_some() && args_of(&m.descriptor) == arg_desc
                })
            {
                let is_static = m.access_flags & 0x0008 != 0;
                let desc = m.descriptor.clone();
                return Ok(Some((Arc::clone(&class), desc, is_static)));
            }
            cur = match class.file.super_class.as_deref() {
                Some(sup) => self.loader.get_or_load(sup),
                None => break,
            };
        }
        Ok(None)
    }
}
