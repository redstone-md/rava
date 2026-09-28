//! invokestatic and invokespecial, plus the worker-side indy refusals.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// The gson native-parse intercept, OUT OF LINE (`#[cold]`, never inlined) so that
/// `invokestatic`'s prologue does not reserve its locals on every call in the program.
/// `Ok(Some(v))` = handled, push `v`; `Ok(None)` = fall through to the bytecode path,
/// exactly as the inline version fell through.
#[cfg(feature = "next-gen-gc")]
#[cold]
#[inline(never)]
fn gson_intercept(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    args: &[Value],
) -> Result<Option<Value>, VmError> {
        // parseString: read the String arg (non-destructive). parseReader / StrictJsonParser.parse:
        // peek the Reader's remaining buffer WITHOUT consuming, parse, and only consume (advance
        // $pos) once the parse + build succeed — so any miss (malformed / trailing content / a
        // non-buffered reader) falls cleanly through to the bytecode path.
        //
        // ★Target #1 step-1: `StrictJsonParser.parse(Reader)` is the REAL datapack path (all 6351
        // files: SimpleJsonResourceReloadListener/RegistryDataLoader/TagLoader/RecipeManager). Its arg
        // is the RAW Reader (BufferedReader→InputStreamReader→resource-stream $buf chain), so
        // `stream_state` reaches the underlying $buf and we tokenize in Rust — bypassing the gson
        // `JsonReader` bytecode tokenizer entirely. (JsonParser.parseReader's arg is the opaque gson
        // JsonReader wrapper with no $stream link, so that arm usually misses + falls through — which
        // is exactly why the old hook never fired on boot.)
        let text_and_consume: Option<(String, Option<u32>)> = match (owner, name) {
            ("com/google/gson/JsonParser", "parseString") => vm
                .value_to_string(args.first().unwrap_or(&Value::Null))
                .ok()
                .map(|t| (t, None)),
            ("com/google/gson/JsonParser", "parseReader")
            | ("net/minecraft/util/StrictJsonParser", "parse") => {
                crate::natives::stream_state(vm, args.first()).and_then(|(h, buf, pos)| {
                    let bytes = vm.heap.array_bytes(buf).unwrap_or_default();
                    if pos > bytes.len() {
                        return None;
                    }
                    String::from_utf8(bytes[pos..].to_vec()).ok().map(|t| (t, Some(h)))
                })
            }
            _ => None,
        };
        let was_none = text_and_consume.is_none();
        if let Some((text, consume)) = text_and_consume {
            // 20.3 wall-profile: native gson tokenize (the JsonReader-bytecode replacement).
            let _wp = crate::rava_sampler::enter(crate::rava_sampler::NATIVE_BRIDGES);
            // Named so the gson tokenizer is a ROW of the bridge table rather than part of its
            // unattributed remainder - it is the single bridge this category is named after.
            let _bsc = crate::rava_sampler::bridge_scope(crate::rava_sampler::register_bridge(
                "<gson>", "tokenize+parse",
            ));
            // 18.4: fire the one-time parallel tokenize-prepass, then serve THIS file's pre-tokenized
            // AST from the content-keyed cache (bit-exact — tokenize is a pure fn of the text); inline
            // tokenize on a miss (non-/data file, repeat) or when the prepass is off.
            let parsed = {
                let _t = crate::diag::gson_tokenize().start();
                if crate::datapack_cache::enabled() {
                    vm.datapack_prewarm_tokenize();
                    crate::datapack_cache::take(&text).or_else(|| crate::gson_native::parse(&text))
                } else {
                    crate::gson_native::parse(&text)
                }
            };
            if let Some(tree) = parsed {
                crate::frames::native_depth_inc();
                let built = {
                    let _t = crate::diag::gson_build().start();
                    crate::gson_native::build(vm, &tree)
                };
                crate::frames::native_depth_dec();
                if let Some(el) = built? {
                    if let Some(h) = consume {
                        // The whole buffer parsed as one document → mark the reader drained.
                        let end = vm.heap.array_len(match vm.heap.get_field(h, "$buf") {
                            Value::Array(b) => b,
                            _ => 0,
                        }).unwrap_or(0);
                        vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(end as i32));
                    }
                    // Bounded success trace: prove the native tokenizer fires on the real boot path.
                    if crate::env_flag!("RAVA_GSON_TRACE") {
                        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                        let i = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i < 8 {
                            let snip: String = text.chars().take(56).collect();
                            eprintln!("[GSON-NATIVE] #{i} {}.{} native-tokenized {} bytes: {}…", owner, name, text.len(), snip.replace('\n', " "));
                        }
                    }
                    return Ok(Some(el));
                }
            }
        }
        // Miss-diagnostic: the hook matched the method but couldn't peek a buffer (no $buf in
        // the reader's $stream chain) → we fall through to bytecode. Dump the reader chain so we
        // can see which stream shape the datapack `Resource.open()` path produces.
        if std::env::var_os("RAVA_GSON_TRACE").is_some() && was_none {
            static M: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let i = M.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if i < 6 {
                let mut chain = String::new();
                let mut cur = args.first().cloned();
                for _ in 0..8 {
                    match cur {
                        Some(Value::Ref(h)) => {
                            let cn = vm.heap.class_of(h).map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                            let has_buf = matches!(vm.heap.get_field(h, "$buf"), Value::Array(_));
                            chain.push_str(&format!("{}[buf={}]→", cn, has_buf));
                            cur = match vm.heap.get_field(h, "$stream") { Value::Null => None, v => Some(v) };
                        }
                        _ => break,
                    }
                }
                eprintln!("[GSON-MISS] #{i} {}.{} no-peek chain: {}", owner, name, chain);
            }
        }
    
    Ok(None)
}

/// Does the gson native-parse intercept in `invokestatic` test this site? Mirrors that
/// hook's own condition verbatim; a site it could claim is denied a replay plan, because
/// the replay is defined as "the prologue would have done nothing".
fn gson_claimable(owner: &str, name: &str) -> bool {
    #[cfg(feature = "next-gen-gc")]
    {
        gson_native_enabled()
            && (owner == "com/google/gson/JsonParser"
                || (owner == "net/minecraft/util/StrictJsonParser" && name == "parse"))
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let _ = (owner, name);
        false
    }
}

/// The transitional `&SharedVm -> &mut Vm` escape for the invoke* family's genuinely-`&mut`
/// callees — the SAME cast `getstatic_resolve`/`putstatic` spell at their sites, hoisted to
/// ONE definition so every use below reads as a plain call instead of a per-site copy of the
/// raw-pointer expression. Everything else on these paths is already `&self`-safe
/// (`pop_args*`, `push_call_roots`, `resolve_method`, `force_native_collection`, `tlcache`,
/// `ensure_init_shared`, `give_vec` — see each definition), so the function itself runs on
/// the shared reference and only these leaves still need exclusivity until the interpreter
/// loop is `&self`. Bit-identical to the `&mut Vm` call it replaces; only the receiver's
/// provenance differs, and every derivation is consumed at its own call site — none is held
/// across another.
#[inline(always)]
#[allow(clippy::mut_from_ref, invalid_reference_casting)]
fn vm_mut(vm: &crate::vm::thread_context::SharedVm) -> &mut Vm {
    unsafe {
        &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm)
    }
}

/// Put a resolved call on the explicit activation stack. The driver consumes this
/// only after the current opcode loop releases its frame borrow.
#[inline]
fn stage_call(
    vm: &crate::vm::thread_context::SharedVm,
    decl: Arc<crate::class_loader::LoadedClass>,
    name: Arc<str>,
    desc: Arc<str>,
    args: Vec<Value>,
    midx: Option<u32>,
) {
    vm_mut(vm).pending = Some(crate::vmstate::PendingCall {
        decl,
        name,
        desc,
        args,
        midx,
    });
}

/// `invokestatic` — resolve through the owner hierarchy; unresolved = native.
///
/// ⭐**`&SharedVm`** — the tracer bullet for the invoke* family: the function runs on the
/// plain shared reference (`&mut Vm` callers coerce for free), and the ONLY exclusivity
/// left on the path is the `vm_mut` leaves below.
pub(crate) fn invokestatic(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    index: u16,
) -> Result<(), VmError> {
    // ⭐**ONE FACT: THE CALLER CLASS'S IDENTITY.** `Frame::class_detached` is literally
    // `&*Arc::as_ptr(&self.class)`, and the `tlcache` site key built ~85 lines below opened with
    // `Arc::as_ptr(&frame.class) as usize` — the SAME probe, spelled the other way, loaded a
    // second time on every invokestatic that misses the replay. Derived once, threaded to both.
    let cls = unsafe { frame.class_detached() };
    let cptr = cls as *const crate::class_loader::LoadedClass as usize;
    debug_assert_eq!(cptr, std::sync::Arc::as_ptr(&frame.class) as usize);
    // One atomic for the resolved site; the argument count rides along instead of being
    // re-derived from the descriptor string on every call.
    let site_ref = ref_site_ref(cls, index)?;
    // ⭐**RESOLVED-SITE REPLAY** (`RAVA_CALL_PLAN`). Everything between here and the
    // `vm.invoke_idx` at the bottom is a function of `(owner, name, descriptor)`, all three
    // fixed for this site — so once it has been decided, replay it. What is genuinely
    // per-call stays: the class-init latch below, and the trampoline flag, which changes
    // where the call is staged and so must not be short-circuited.
    if call_plan_enabled() {
        if let Some(plan) = site_ref.plan.get() {
            if plan.decl.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
                let args = pop_args_n(vm, frame, site_ref.argc as usize)?;
                #[cfg(feature = "next-gen-gc")]
                let _gc_root = push_call_roots(vm, None, &args);
                if vm.trampoline {
                    stage_call(
                        vm,
                        Arc::clone(&plan.decl),
                        site_ref.name.clone(),
                        site_ref.descriptor.clone(),
                        args,
                        Some(plan.midx),
                    );
                } else if let Some(ret) = vm_mut(vm).invoke_idx(
                    &plan.decl,
                    Some(plan.midx),
                    &site_ref.name,
                    &site_ref.descriptor,
                    args,
                )? {
                    frame.push(ret);
                }
                return Ok(());
            }
        }
    }
    let (owner, name, desc): (&str, &str, &str) =
        (&site_ref.owner, &site_ref.name, &site_ref.descriptor);
    let args = pop_args_n(vm, frame, site_ref.argc as usize)?;
    #[cfg(feature = "next-gen-gc")]
    let _gc_root = push_call_roots(vm, None, &args);
    // ⭐**ONE FACT: "CAN THE GSON INTERCEPT CLAIM THIS SITE?"** — a pure function of the site's
    // FIXED `(owner, name)` plus a process gate, and it was derived TWICE per call: once as the
    // inline condition of the block immediately below, and again ~110 lines down, where the
    // replay-plan installation asks `gson_claimable(owner, name)` the same question. That
    // helper's own doc says it "Mirrors that hook's own condition verbatim" — the textbook
    // shape of one lookup written two different ways, with the two copies free to drift.
    // Derived once, here, and threaded to both. (Cheap either way — `gson_native_enabled()` is
    // an op-gate bit that is clear unless `RAVA_GSON_NATIVE` is set — but a fact derived twice
    // is a fact derived twice, and the second copy could not be checked against the first.)
    let gson_claim = gson_claimable(owner, name);
    // GSON NATIVE-PARSE fast-path (RAVA_GSON_NATIVE, default off) — see `gson_intercept`.
    //
    // ⭐**OUTLINED, AND THE OUTLINING IS THE POINT.** This block is ~110 lines holding
    // `String`s, a `format!`, a chain walk and several `Vec`s, and it was inline in
    // `invokestatic` — a function on the path row 395 prices at 105 ns for a call whose
    // callee is free. A block's locals are reserved by the PROLOGUE, so every call in the
    // program paid this one's stack frame whether or not the flag was even set.
    #[cfg(feature = "next-gen-gc")]
    if gson_claim {
        // ⭐**IS THIS INTRINSIC REACHABLE FROM COMPILED CODE?** The hook lives in the
        // INTERPRETER's `invokestatic` and there is no JIT-side twin and no cascade entry, so a
        // caller that has compiled calls `StrictJsonParser.parse` straight through `jit_call_body`
        // and never offers the site here. In a probe whose caller is `main` (entered once, never
        // tiered) it fires; in a boot, the callers -- `scanDirectory`,
        // `loadElementFromResource` -- run thousands of times and compile. Counted rather than
        // reasoned about.
        if let Some(el) = gson_intercept(vm_mut(vm), owner, name, &args)? {
            crate::diag::gson_native_served().inc();
            frame.push(el);
            return Ok(());
        }
        crate::diag::gson_native_missed().inc();
    }
    // Cooperative yield: under the green-thread scheduler (trampoline mode),
    // Thread.sleep is a yield point — park this green thread and let others run —
    // not a real OS sleep. pc is advanced by the op dispatcher after we return, so
    // the loop top sees `yield_request` and suspends the frame at a clean boundary.
    if vm.trampoline && owner == "java/lang/Thread" && name == "sleep" {
        let ms = match args.first() {
            Some(Value::Long(m)) => *m,
            Some(Value::Int(m)) => *m as i64,
            _ => 0,
        };
        vm_mut(vm).yield_request = Some(ms);
        return Ok(());
    }
    // HashBiMap.create() etc. — serve from the native layer (see invokevirtual).
    // Static dispatch is monomorphic per call site → cache the resolution in
    // call_cache (separate from virtual dispatch's inline_cache).
    // ★ THE FORCE-NATIVE TEST BELONGS BEHIND THE SITE CACHE, NOT IN FRONT OF IT.
    //
    // `force_native_collection` is seven-plus `starts_with`/`contains` scans, and it ran on
    // EVERY invokestatic — including a cached site, and including a static call into a plain
    // user class that no native could ever serve. Caller-attributed profiling of ONE interpreted
    // static call (ledger row 185) puts `str::pattern::next_match` at 3.40%, `memchr` 1.15% and
    // `force_native_collection` itself at 1.03% — on a call to a method whose whole body is one
    // add. Its verdict is a pure function of `(owner, name, descriptor)`, all three FIXED for a
    // call site, so computing it once and letting the cache carry it is behaviour-identical: a
    // forced-native site simply caches `None`, which is exactly what the front test produced.
    let resolved = {
        // `cptr` — the caller class pointer derived once at the top of this function.
        let site = (cptr, index);
        match crate::tlcache::call_get(site) {
            Some(m) => m,
            None => {
                let r = if force_native_collection(vm, &owner, &name, &desc) {
                    None
                } else {
                    vm.resolve_method(&owner, &name, &desc)?
                };
                crate::tlcache::call_put(site, r.clone());
                r
            }
        }
    };
    match resolved {
        Some(decl) => {
            // JVM §5.5: the first execution of a static method INITIALIZES its
            // declaring class. rava triggered <clinit> on getstatic/putstatic/new
            // but never on invokestatic, so a class whose only entry point is a
            // static method ran with default (null/0) statics — and a <clinit>
            // that should have thrown never did (a netty
            // Slf4JLoggerFactory$NopInstanceHolder-style init error simply
            // vanished, leaving half-initialised state to fail later, silently).
            // A latch read makes the hot path one boolean check. Gate on
            // clinit_done (not `initialized`, which latches at clinit START):
            // an ERRONEOUS class (clinit threw) never completes, so every later
            // static call re-enters ensure_init and throws NCDFE; a mid-clinit
            // recursive static call still passes via ensure_init's start-latch.
            if !decl.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
                // `ensure_init_shared` — the `&self`-safe twin putstatic already calls; its
                // cold arm's cast lives inside it. We are here only while `clinit_done` is
                // clear, so its `settled()` shortcut can never fire — behaviour identical.
                vm.ensure_init_shared(&decl)?;
            }
            // The site remembers which method it resolved to, so the repeat call skips the name
            // hash + descriptor memcmp inside `invoke`. Computed BEFORE the trampoline staging
            // below, not after: the explicit-stack machine used to return here first and so never
            // reached this memo — `build_frame` then re-found the method by name on every
            // trampolined call, which is exactly what the memo exists to stop.
            let dptr = std::sync::Arc::as_ptr(&decl) as usize;
            let midx = match site_ref.resolved.get() {
                Some(&(cp, i)) if cp == dptr => Some(i),
                Some(_) => None,
                None => {
                    // ⭐**THE SAME LOOKUP, WRITTEN TWO DIFFERENT WAYS.** This was an open-coded
                    // linear scan of `decl`'s method table with two string compares per element
                    // (30-300 methods for the JDK and Minecraft classes on this path), while
                    // `invokespecial` fifty lines down asked for the identical answer through
                    // `tlcache::method_index` — which reads the open-addressed `(name,
                    // descriptor) -> index` table `ClassFile` already owns, and whose OFF arm IS
                    // this exact scan. Its doc pins the equivalence including the illegal-
                    // duplicate case: it linear-probes, "so the LOWEST index is found, exactly as
                    // `Iterator::position` did". One spelling now, on the cheaper side.
                    //
                    // POPULATION: cold — once per invokestatic site (the `OnceLock` below latches
                    // the answer). Counted as a two-ways hazard removed, not as a hot-path win.
                    let found = crate::tlcache::method_index(&decl, name, desc);
                    if let Some(i) = found {
                        let _ = site_ref.resolved.set((dptr, i));
                    }
                    found
                }
            };
            // Install after the site's intercept decisions, including on the explicit
            // stack; later staged calls can reuse the resolved declaration and index.
            // Replay runs before the intercept chain on later executions; any new
            // invokestatic intercept must be excluded by this eligibility check.
            if call_plan_enabled() && !crate::parallel::gilfree() {
                if let (Some(i), false) = (midx, gson_claim) {
                    let _ = site_ref
                        .plan
                        .set(crate::class_loader::CallPlan { decl: decl.clone(), midx: i });
                }
            }
            if vm.trampoline {
                stage_call(
                    vm,
                    decl,
                    site_ref.name.clone(),
                    site_ref.descriptor.clone(),
                    args,
                    midx,
                );
                return Ok(());
            }
            if let Some(ret) = vm_mut(vm).invoke_idx(&decl, midx, name, desc, args)? {
                frame.push(ret);
            }
            Ok(())
        }
        // The native layer only BORROWS the buffer, so capture its result, put the
        // buffer back in the pool, and only then propagate — a `?` here would have
        // dropped it (see `argbuf_recycle`).
        None => match {
            crate::vecgrow::event(3);
            let r = crate::natives::try_invoke(vm_mut(vm), &owner, &name, &desc, &args);
            if argbuf_recycle() {
                let mut a = args;
                a.clear();
                vm.give_vec(a);
            }
            r?
        } {
            Some(ret) => { if let Some(v) = ret { frame.push(v); } Ok(()) }
            None => {
                let sig = format!("invokestatic {owner}.{name}{desc}");
                if collect_missing(&sig) {
                    if let Some(d) = default_for_return(&desc) { frame.push(d); }
                    Ok(())
                } else {
                    let msg = format!("{sig} (from {})", frame.class.file.this_class);
                    Err(unlinked_native(vm_mut(vm), msg))
                }
            }
        },
    }
}

/// `invokespecial` — constructors / private / super calls. Unresolvable owner
/// (e.g. `java/lang/Object.<init>`) is a no-op after consuming the receiver.
///
/// ⭐**`&SharedVm`** — the invoke* family's second tracer bullet, the same shape as
/// `invokestatic` above: the function runs on the plain shared reference (`&mut Vm` callers
/// coerce for free), and the ONLY exclusivity left on the path is the `vm_mut` leaves below
/// (`invoke_idx`, the fastutil `<init>` dispatch, the trampoline `pending` staging,
/// `natives::try_invoke`). Everything else — `pop_recv_args_n`, `push_call_roots`,
/// `force_native_collection`, `tlcache`, `resolve_method`, `give_vec` — is already
/// `&self`-safe. There is no NPE arm and no `ensure_init` on this path: every error is a
/// callee's own `VmError`, propagated unchanged.
pub(crate) fn invokespecial(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    index: u16,
) -> Result<(), VmError> {
    // ⭐**ONE FACT: THE CALLER CLASS'S IDENTITY** — as in `invokestatic`. `class_detached()` and
    // the `Arc::as_ptr(&frame.class)` of the `tlcache` site key below are the same probe; one
    // load now serves both.
    let cls = unsafe { frame.class_detached() };
    let cptr = cls as *const crate::class_loader::LoadedClass as usize;
    debug_assert_eq!(cptr, std::sync::Arc::as_ptr(&frame.class) as usize);
    // One atomic for the resolved site; the argument count rides along instead of being
    // re-derived from the descriptor string on every call.
    let site_ref = ref_site_ref(cls, index)?;
    // ⭐**RESOLVED-SITE REPLAY**, as in `invokestatic` — and this is the arm that matters most
    // for allocation-shaped code: `new Point(i, i*2, i*3)` is TWO invokespecials (the class's
    // own `<init>` and the implicit `Object.<init>` super), and row 395 measures those two
    // constructor frames at 400 ns while the `new` opcode and the allocation together are 40.
    // Taken only when the class is already initialised, so the shortcut can never run a
    // `<clinit>` earlier than the path it replaces; otherwise it falls through verbatim.
    if call_plan_enabled() {
        if let Some(plan) = site_ref.plan.get() {
            if plan.decl.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
                // ⭐ONE MOVE. The receiver and the arguments are one contiguous run of `argc + 1`
                // operands (JVMS 6.5) and the callee's locals are that run — see
                // `pop_recv_args_n`. This used to be pop-N, pop-1, then a memmove of the N back
                // one slot. Rooting the whole buffer roots receiver-then-args, the same set in
                // the same order the split call produced, with no GC point in between.
                let locals = pop_recv_args_n(vm, frame, site_ref.argc as usize)?;
                #[cfg(feature = "next-gen-gc")]
                let _gc_root = push_call_roots(vm, None, &locals);
                if vm.trampoline {
                    stage_call(
                        vm,
                        Arc::clone(&plan.decl),
                        site_ref.name.clone(),
                        site_ref.descriptor.clone(),
                        locals,
                        Some(plan.midx),
                    );
                } else if let Some(ret) = vm_mut(vm).invoke_idx(
                    &plan.decl,
                    Some(plan.midx),
                    &site_ref.name,
                    &site_ref.descriptor,
                    locals,
                )? {
                    frame.push(ret);
                }
                return Ok(());
            }
        }
    }
    let (owner, name, desc): (&str, &str, &str) =
        (&site_ref.owner, &site_ref.name, &site_ref.descriptor);
    // ⭐**ONE MOVE, NOT THREE STEPS.** `pop_args_n` + `frame.pop()` + `locals.insert(0, receiver)`
    // took the callee's locals apart and put them back together; the receiver was already
    // directly below the arguments on the operand stack (JVMS 6.5), which is exactly the layout
    // wanted. `pop_recv_args_n` moves the whole `argc + 1` run once. It keeps the pooled buffer
    // for the same reason the old comment here gave — the buffer `pop_args_n` reserved `n + 1`
    // slots in is the one that becomes the frame — and it removes the memmove that reservation
    // existed to absorb. POPULATION: every invokespecial that does not take the replay above,
    // i.e. every constructor call in the program plus its implicit `Object.<init>` super.
    //
    // Roots the whole buffer instead of (receiver, args) separately: same values, same order
    // (receiver first), and no collection point falls between the move and the rooting.
    let locals = pop_recv_args_n(vm, frame, site_ref.argc as usize)?;
    #[cfg(feature = "next-gen-gc")]
    let _gc_root = push_call_roots(vm, None, &locals);
    // ⭐**ONE FACT: "IS THIS THE FASTUTIL INT-SET `<init>` THE HOOK BELOW CLAIMS?"** — a pure
    // function of the site's FIXED `(owner, name)`, and it was derived TWICE per call: once as
    // the guard of the hook immediately below, and again ~70 lines down as `fastutil_claimable`,
    // where the replay-plan installation asks the identical question. `fastutil::is_int_set` is
    // a linear scan of five 40-character class names, so the pair cost up to ten string compares
    // per call.
    //
    // POPULATION: every invokespecial that does NOT take the replay above — and that set is
    // permanent, not just first-call, because a site whose resolution is `None` never installs a
    // plan. `java/lang/Object.<init>()V` is exactly that site, and javac emits it as the implicit
    // super of every constructor in the program, so the five-name scan ran on every `new`,
    // forever, to answer "no" about a class named `java/lang/Object`.
    //
    // ⚠**PREFILTER, REJECT-ONLY.** All five entries of `fastutil::INT_SETS` begin with the byte
    // `i` (`it/unimi/dsi/fastutil/ints/Int…`), so an owner whose first byte is not `i` cannot be
    // in the list: the test can only turn a `true` into a `false`, never the reverse, and the
    // `debug_assert_eq!` checks it against the unfiltered predicate on every call in a debug
    // build. Behind the kill switch the prefilter is skipped and `is_int_set` runs exactly as it
    // always did.
    let fast = fast_callpath();
    let fastutil_int_init = name == "<init>"
        && (!fast || owner.as_bytes().first() == Some(&b'i'))
        && crate::fastutil::is_int_set(owner);
    debug_assert_eq!(
        fastutil_int_init,
        name == "<init>" && crate::fastutil::is_int_set(owner),
        "the fastutil owner first-byte prefilter must only ever REJECT"
    );
    // fastutil int-set <init> fast-path: model as a sorted $elems int array
    // instead of running the real RB-tree/hash bytecode (whose addAll(Collection)
    // path is fragile at our JDK boundary). May drain a Collection → gate GC.
    if fastutil_int_init {
        crate::frames::native_depth_inc();
        let r = crate::fastutil::dispatch(vm_mut(vm), "<init>", &locals);
        crate::frames::native_depth_dec();
        if r?.is_some() { return Ok(()); }
    }
    // invokespecial is monomorphic per call site → cache in call_cache.
    //
    // ★ THE FORCE-NATIVE TEST BELONGS BEHIND THE SITE CACHE, exactly as the comment above
    // `invokestatic`'s cache says — and here it was still in FRONT of it, so seven-plus
    // `starts_with`/`contains` scans over the owner name ran on EVERY invokespecial. That is
    // every constructor call in the program. An interpreter-only profile of the worldgen window
    // puts `memchr` at 4.8%, the largest row that is not interpretation.
    //
    // Behaviour is identical for the same reason it was there: the verdict is a pure function of
    // `(owner, name, descriptor)`, all three FIXED for a call site, and a forced-native site
    // simply caches `None` — which is what the front test produced.
    let resolved = {
        // `cptr` — the caller class pointer derived once at the top of this function.
        let site = (cptr, index);
        let r = match crate::tlcache::call_get(site) {
            Some(m) => m,
            None => {
                let r = if force_native_collection(vm, &owner, &name, &desc) {
                    None
                } else {
                    vm.resolve_method(&owner, &name, &desc)?
                };
                crate::tlcache::call_put(site, r.clone());
                r
            }
        };
        // A super.put()/super.get() into a fastutil object map → native (the base
        // bytecode would run over uninitialised internal arrays).
        if r.as_ref().is_some_and(|d| route_native_map(d)) { None } else { r }
    };
    match resolved {
        Some(decl) => {
            // The site remembers which method it resolved to, exactly as `invokestatic` does.
            // Without it every constructor call re-finds the method BY NAME inside `invoke`: a
            // hash of the name plus a descriptor compare across the overload set, for something
            // fixed once the site has resolved. `invokespecial` is monomorphic per site (see the
            // cache comment above), and the class-pointer check is what keeps it sound if the
            // declaring class ever resolves elsewhere.
            let dptr = std::sync::Arc::as_ptr(&decl) as usize;
            let midx = match site_ref.resolved.get() {
                Some(&(cp, i)) if cp == dptr => Some(i),
                Some(_) => None,
                None => {
                    let found = crate::tlcache::method_index(&decl, name, desc);
                    if let Some(i) = found {
                        let _ = site_ref.resolved.set((dptr, i));
                    }
                    found
                }
            };
            // Only ordinary bytecode constructors can be replayed; the fastutil
            // constructor interception has already been ruled out above.
            // Keep this exclusion in sync with any new owner-specific intercept.
            if call_plan_enabled() && !crate::parallel::gilfree() {
                if let (Some(i), false) = (midx, fastutil_int_init) {
                    let _ = site_ref
                        .plan
                        .set(crate::class_loader::CallPlan { decl: decl.clone(), midx: i });
                }
            }
            if vm.trampoline {
                stage_call(
                    vm,
                    decl,
                    site_ref.name.clone(),
                    site_ref.descriptor.clone(),
                    locals,
                    midx,
                );
                return Ok(());
            }
            if let Some(ret) = vm_mut(vm).invoke_idx(&decl, midx, &name, &desc, locals)? {
                frame.push(ret);
            }
            Ok(())
        }
        // JDK <init> (e.g. InputStreamReader wrapping a stream) routes to the
        // native layer; truly unmodelled super/Object <init> is a no-op.
        // As in `invokestatic`: recycle the borrowed buffer before propagating.
        None => {
            // ★ `java/lang/Object.<init>()V` IS EVERY `new` IN THE PROGRAM.
            //
            // javac emits it as the implicit super-call of every constructor whose class extends
            // Object, so a `new` costs two invokespecials and the second one is always this. It
            // can never be served by a native — Object carries no state in this engine — and the
            // arm below already ends at `None => Ok(())`, a no-op. What it pays to get there is
            // the whole owner cascade: a PC profile of `bench/CtorBench.java` puts
            // `owner_bits::bits` at 9.7%, `memchr` 4.2%, `dispatch_owner_cascade` 3.8%,
            // `owner_families` 3.4%, `owner_table`+`owner_table2` 5.4% and `try_invoke` 2.1% —
            // **28.7% of the constructor path spent discovering there is nothing to call.**
            //
            // Two exact string compares decide it, and they are on the SITE's fixed owner and
            // name, not on anything about the workload.
            // `java/lang/Record.<init>()V` is the same no-op super, for every record — and this
            // tree already says so twice: `native_contract.rs` marks it PURE with the comment
            // "java.lang.Record's constructor is the same no-op super()", and the counted-loop
            // fork admits exactly this pair as "both guaranteed-empty JDK ctors"
            // (`vm/fork_apply.rs`). MC's registries, codecs and worldgen configs are records.
            if name == "<init>" && matches!(owner, "java/lang/Object" | "java/lang/Record") {
                if argbuf_recycle() {
                    let mut a = locals;
                    a.clear();
                    vm.give_vec(a);
                }
                return Ok(());
            }
            match {
            crate::vecgrow::event(4);
            let r = crate::natives::try_invoke(vm_mut(vm), &owner, &name, &desc, &locals);
            if argbuf_recycle() {
                let mut a = locals;
                a.clear();
                vm.give_vec(a);
            }
            r?
        } {
            Some(ret) => { if let Some(v) = ret { frame.push(v); } Ok(()) }
            None => Ok(()),
            }
        }
    }
}

/// `invokedynamic` — currently the `StringConcatFactory` bootstraps
/// (`makeConcat` / `makeConcatWithConstants`), which Java 21 emits for the `+`
/// operator. Other bootstraps (e.g. LambdaMetafactory) surface as the next
/// frontier rather than guessing.
/// GILFREE-RINGS: may a gil-free worker run `invokedynamic` at this site WITHOUT
/// mutating any shared VM table? Read-only predicate — every check is a lookup.
///
/// WARM means "servable without publication", NOT "this thread has been here
/// before". The distinction is the whole predicate: the resolved-plan cache is
/// THREAD-LOCAL, so a worker's first execution of any site is cache-cold, and a
/// cache-only definition of warm denies the first task on every worker — which in
/// a phase of three tasks on three threads is every task.
///
/// Three shapes qualify:
///
/// 1. A site already linked as a NON-CAPTURING singleton — served at the top of
///    `invokedynamic` by a pure `indy_singletons` read plus a stack push.
/// 2. A site whose plan is cached in THIS thread and is a CAPTURING lambda.
/// 3. A site not yet cached here whose RESOLUTION is itself mutation-free: the
///    bootstrap walk reads the immutable constant pool, and the LambdaMetafactory
///    arm's only shared touch is interning the impl's owner/name/descriptor — a
///    pure lookup once those three strings are in the pool (they are, from
///    whichever thread first linked this site). The resolved plan is then cached
///    thread-locally. No other bootstrap qualifies: Concat/Record/Switch `ldc`
///    their static args, which can intern a Class object.
///
/// CAPTURING is required throughout, so `invokedynamic_assemble` takes the
/// closure-minting arm and never reaches the singleton publication. What that arm
/// touches is all thread-private: `alloc_lambda` routes through `put_object`'s
/// worker branch into this worker's pre-reserved handle range, and the `$fi` box
/// lands in its pre-reserved `field_box` slice. Its two interner consultations
/// (`$Lambda` as a class name, `$fi` as a field name) must be proven HITS — both
/// are populated by the first closure of the boot, but proving beats assuming.
/// `lambda_dedup` is a shared map on the same arm, so its flag disables the whole
/// predicate rather than being special-cased.
/// `None` = servable. `Some(reason)` names the exact sub-check that refused, so a
/// denial is attributable in the lane report instead of one opaque `indy`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn worker_indy_refusal(
    vm: &Vm,
    caller: &crate::class_loader::LoadedClass,
    cp: u16,
) -> Option<&'static str> {
    let site = (caller as *const crate::class_loader::LoadedClass as usize, cp);
    // The ARENA singleton table counts as "already linked" exactly as the handle one does —
    // both mean the site's object exists and the evaluation is a pure read plus a push. Under
    // RAVA_ARENA_LAMBDA_SINGLETON only the arena table is filled, so checking the handle table
    // alone declared every such site UNLINKED, sent the lane down the resolving path, and
    // produced the NullPointerException in resource loading that killed the boot.
    #[cfg(feature = "next-gen-gc")]
    if vm.arena_indy_singletons.contains_key(&site) {
        return None;
    }
    if vm.indy_singletons.contains_key(&site) {
        return None;
    }
    // Mirrors the `dedup_key` guard in `invokedynamic_assemble`: the rings lane
    // skips dedup outright, so its shared map is untouched. Any OTHER gil-free
    // caller (the chunk-fill / bitstorage COW forks) still runs the dedup arm and
    // must stay denied.
    if lambda_dedup_enabled() && !crate::gilfree_cf::in_lane() {
        return Some("indy(lambda-dedup map)");
    }
    if !vm.heap.class_is_interned("$Lambda") {
        return Some("indy($Lambda uninterned)");
    }
    if vm.heap.field_id_opt("$fi").is_none() {
        return Some("indy($fi uninterned)");
    }
    if crate::tlcache::indy_warm_capturing_lambda(site) {
        return None;
    }
    worker_indy_resolve_is_pure(vm, caller, cp)
}

/// Shape 3 of `worker_indy_refusal`: would resolving this site from scratch touch
/// only immutable state + interner HITS? Mirrors the LambdaMetafactory arm of
/// `invokedynamic` read-for-read.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn worker_indy_resolve_is_pure(
    vm: &Vm,
    caller: &crate::class_loader::LoadedClass,
    cp: u16,
) -> Option<&'static str> {
    let pool = &caller.file.pool;
    let Some((bootstrap, _, desc)) = pool.invoke_dynamic(cp) else {
        return Some("indy(pool undecodable)");
    };
    if desc.starts_with("()") {
        return Some("indy(non-capturing singleton)");
    }
    let Some(bm) = caller.file.bootstrap_methods.get(bootstrap as usize) else {
        return Some("indy(pool undecodable)");
    };
    let Some(handle) = pool.method_handle(bm.handle_index) else {
        return Some("indy(pool undecodable)");
    };
    match handle.owner {
        "java/lang/invoke/LambdaMetafactory" => {}
        "java/lang/invoke/StringConcatFactory" => return Some("indy(string concat)"),
        "java/lang/runtime/ObjectMethods" => return Some("indy(record bootstrap)"),
        "java/lang/runtime/SwitchBootstraps" => return Some("indy(switch bootstrap)"),
        _ => return Some("indy(unknown bootstrap)"),
    }
    let Some(&mh) = bm.args.get(1) else {
        return Some("indy(pool undecodable)");
    };
    let Some((_, m)) = pool.method_handle_full(mh) else {
        return Some("indy(pool undecodable)");
    };
    if !vm.heap.class_is_interned(m.owner) {
        return Some("indy(impl owner uninterned)");
    }
    if !vm.heap.class_is_interned(m.name) {
        return Some("indy(impl name uninterned)");
    }
    if !vm.heap.class_is_interned(m.descriptor) {
        return Some("indy(impl descriptor uninterned)");
    }
    None
}
