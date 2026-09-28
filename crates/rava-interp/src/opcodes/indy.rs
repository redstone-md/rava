//! invokedynamic: LINK the call site once, then mint from the linked entry.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;
use std::cell::RefCell;
use std::rc::Rc;

/// **DEFAULT ON — kill switch `RAVA_NO_FAST_INDY=1`.**
///
/// With it set, `invokedynamic` takes the pre-linkage path: the singleton probes and
/// `tlcache::indy_get` in their old order, and a `LinkedSite` derived FRESH on every execution
/// (which is what the old code did inline — deep-clone the `Plan`, allocate a `String` for the
/// descriptor, re-walk it for the arity, the concat parameter types and the `$fi` name). One
/// assembly implementation serves both, so the switch turns off the CACHE, not the code.
fn fast_indy_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_FAST_INDY").is_none())
}

/// **A CALL SITE IS LINKED ONCE, AND EVERYTHING IN HERE IS A CONSTANT OF THAT SITE.**
///
/// JVMS 5.4.3.6: an `invokedynamic` instruction resolves its bootstrap method exactly once and
/// the `CallSite` it produces is bound to that instruction for the life of the class. This engine
/// agreed in principle — `tlcache::indy_put` has cached the resolved `Plan` per site for a long
/// time — and then re-derived most of the linkage on EVERY execution anyway:
///
///   * `tlcache::indy_get` handed back the plan by DEEP CLONE and the descriptor as a fresh
///     `String`, so a concat site allocated its recipe `String` + its constants `Vec` + the
///     descriptor on every single `"a" + i + "b"`;
///   * `invokedynamic_assemble` then walked that descriptor three more times — `count_args` for a
///     diagnostic that is armed only under `RAVA_DIAG`, `descriptor::arg_count` inside `pop_args`,
///     and `concat_param_types` (another `Vec`) for the concat arm;
///   * the handle-lambda arm rebuilt the `$fi` name — the indy descriptor's RETURN type, a
///     per-site constant — into a new `Arc<str>` per closure;
///   * the arena arm re-probed a second thread-local map for the site's `ArenaLambdaShape` and
///     re-published that shape to `jit::lambda_shape` (a failing `lock cmpxchg`) per closure.
///
/// `RAVA_DIAG=indy` counts **2,537,018 LambdaMetafactory assemblies in one datapack run**, of
/// which 6,188 capture nothing. Every item above is that population paying, over and over, for a
/// bootstrap that happened once. They are all fields now, filled by the link.
pub(crate) struct LinkedSite {
    /// The resolved bootstrap result. Read by reference from here on — nothing clones it.
    plan: Plan,
    /// The indy descriptor, owned once instead of `to_string()`-ed per execution.
    desc: Box<str>,
    /// `descriptor::arg_count(desc)` — the count `pop_args` used to re-derive per call.
    argc: usize,
    /// Can this site EVER publish a `ConstantCallSite` singleton? Only a `LambdaMetafactory`
    /// site with no dynamic arguments does (see the mint below), and 2,530,830 of those 2.5 M
    /// assemblies are capturing — so this flag is what keeps the two singleton-map probes off
    /// the path that never had anything to find in them.
    singleton: bool,
    /// `concat_param_types(desc)` for a `Plan::Concat`, empty otherwise.
    concat_types: Box<[u8]>,
    /// The functional-interface class name (`$fi`) as the exact `Value` the handle mint stores,
    /// so a birth clones a refcount instead of allocating an `Arc<str>` from the descriptor.
    fi: Option<Value>,
    /// The site's arena closure shape, resolved on FIRST arena mint and not before: deriving it
    /// registers an arena class and interns the impl site, which must not happen at a site whose
    /// arena arm never runs (`RAVA_NO_ARENA_LAMBDA`).
    #[cfg(feature = "next-gen-gc")]
    shape: RefCell<Option<Rc<crate::nextgen::ArenaLambdaShape>>>,
    /// Has this site's shape been published to `jit::lambda_shape` yet? That table is
    /// first-writer-wins, so every later execution was paying a hash, a nibble pack and a failing
    /// `compare_exchange` to be told what it already knew.
    #[cfg(feature = "next-gen-gc")]
    shape_published: std::cell::Cell<bool>,
}

impl LinkedSite {
    /// LINK: derive every per-site constant from the resolved plan + descriptor. Cold path —
    /// runs once per (thread, site) with the cache on.
    fn link(plan: Plan, desc: String) -> LinkedSite {
        // `descriptor::arg_count`, NOT `argcall::count_args`: it is the function `pop_args` calls,
        // and `pop_args_n(argc)` must consume exactly the operands `pop_args(desc)` would have.
        // (The two are byte-identical readers; using the one the consumer uses removes the
        // question rather than answering it.)
        let argc = crate::descriptor::arg_count(&desc);
        let is_lambda = matches!(&plan, Plan::Lambda { .. });
        let concat_types: Box<[u8]> = if matches!(&plan, Plan::Concat { .. }) {
            concat_param_types(&desc).into_boxed_slice()
        } else {
            Vec::new().into_boxed_slice()
        };
        // Exactly the derivation the handle mint used to run per closure: the text after the LAST
        // ')', unwrapped from `L...;`. A descriptor whose return is primitive or an array yields
        // none, and the mint then stores no `$fi` — same as before.
        let fi = if is_lambda {
            desc.rsplit(')')
                .next()
                .and_then(|r| r.strip_prefix('L').and_then(|s| s.strip_suffix(';')))
                .map(|s| Value::Str(s.into()))
        } else {
            None
        };
        LinkedSite {
            plan,
            desc: desc.into_boxed_str(),
            argc,
            singleton: is_lambda && argc == 0,
            concat_types,
            fi,
            #[cfg(feature = "next-gen-gc")]
            shape: RefCell::new(None),
            #[cfg(feature = "next-gen-gc")]
            shape_published: std::cell::Cell::new(false),
        }
    }

    /// The site's arena closure shape, resolved at most once per (thread, site). `arena_lambda_shape`
    /// memoizes too, but in a SECOND thread-local map keyed by the same site — this holds the answer
    /// on the entry we already have in hand, so the mint costs no probe at all.
    #[cfg(feature = "next-gen-gc")]
    fn arena_shape(
        &self,
        site: (usize, u16),
        impl_site: &std::sync::Arc<crate::LambdaSite>,
    ) -> Rc<crate::nextgen::ArenaLambdaShape> {
        // Clone out of the cell and DROP the borrow before resolving: `arena_lambda_shape` takes
        // global locks (the class registry, the site interner) and this must not be re-entered
        // holding a `RefCell`.
        let hit = self.shape.borrow().clone();
        if let Some(s) = hit {
            return s;
        }
        let s = arena_lambda_shape(site, &self.desc, impl_site);
        *self.shape.borrow_mut() = Some(s.clone());
        s
    }
}

/// **THE LINKED-SITE TABLE.** A direct-mapped front over a thread-local map, the same shape (and
/// for the same reason) as `tlcache`'s pointer-keyed front: an interpreted loop re-executes ONE
/// indy site over and over, so the front answers a repeat without hashing at all, and the map
/// behind it holds the rest.
///
/// ⚠**SIZE, stated because this tree has already paid for getting it wrong:** the front is
/// `INDY_L1` = 64 slots of `Option<(usize, u16, Rc<LinkedSite>)>` = 24 bytes each =
/// **1,536 bytes** per thread, plus the map. A per-thread table that grew 2.5 KB → 14 KB measured
/// **+6.8% WORSE** here than the allocation it removed, so the front deliberately stays inside
/// L1d beside the caches that already live there; the map absorbs capacity instead.
const INDY_L1: usize = 64;

thread_local! {
    /// Direct-mapped front. Slots carry the FULL key (class pointer + cp index) and are compared
    /// against it, so a collision MISSES rather than serving another site's linkage — the
    /// address-keyed-cache rule this engine paid an access violation to learn.
    static LINKED_L1: RefCell<Vec<Option<(usize, u16, Rc<LinkedSite>)>>> =
        const { RefCell::new(Vec::new()) };
    /// The linkage itself. Insert-once, never removed, never mutated after publication —
    /// so a `Rc` handed out of it stays valid for as long as this thread does, which is the
    /// same lifetime `tlcache`'s indy plan cache already assumes.
    ///
    /// THREAD-LOCAL for the reason `nextgen::pinned_lambda_sites` documents: the canonical
    /// `Arc<LambdaSite>` inside a `Plan::Lambda` is address-keyed by dispatch caches, and a
    /// worker thread's exit must not free one that a cache still names. Sharing this table
    /// across threads would need that argument re-made; keeping it per-thread inherits it.
    static LINKED: RefCell<crate::FastMap<(usize, u16), Rc<LinkedSite>>> =
        RefCell::new(crate::FastMap::default());
}

/// The front's index. Same mix as `jit::lambda_shape::slot` — one definition of "how a site
/// scatters" is easier to reason about than two.
#[inline]
fn l1_slot(site: (usize, u16)) -> usize {
    let k = (site.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (site.1 as u64).wrapping_mul(0x517C_C1B7);
    ((k >> 19) as usize) & (INDY_L1 - 1)
}

#[inline]
fn l1_fill(site: (usize, u16), e: &Rc<LinkedSite>) {
    let i = l1_slot(site);
    LINKED_L1.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_empty() {
            c.resize(INDY_L1, None);
        }
        c[i] = Some((site.0, site.1, e.clone()));
    });
}

/// The linked entry for `site`, if this thread has already linked it.
#[inline]
fn linked_get(site: (usize, u16)) -> Option<Rc<LinkedSite>> {
    let i = l1_slot(site);
    let front = LINKED_L1.with(|c| match c.borrow().get(i) {
        Some(Some((cls, cp, e))) if *cls == site.0 && *cp == site.1 => Some(e.clone()),
        _ => None,
    });
    if front.is_some() {
        return front;
    }
    let hit = LINKED.with(|m| m.borrow().get(&site).cloned())?;
    l1_fill(site, &hit);
    Some(hit)
}

fn linked_put(site: (usize, u16), e: &Rc<LinkedSite>) {
    LINKED.with(|m| {
        m.borrow_mut().insert(site, e.clone());
    });
    l1_fill(site, e);
}

/// The transitional `&SharedVm -> &mut Vm` escape for the indy family's genuinely-`&mut`
/// callees — the SAME consolidated cast the invoke* family already spells (see
/// `invoke.rs::vm_mut`). Everything else on the indy paths is already `&self`-safe
/// (`worker_indy_refusal`, `linked_singleton`, `tlcache`, `pop_args_n`, `heap.intern_class`,
/// `heap.put_field`, `heap.class_of`, `get_field_syn`), so both functions run on the plain
/// shared reference and only the leaves below still need exclusivity until the interpreter
/// loop is `&self`. Bit-identical to the `&mut Vm` call each replaces; every derivation is
/// consumed at its own call site — none is held across another. The shared writes behind
/// these leaves (the dedup/singleton tables, `gc_protect` rooting, TLAB/arena alloc) are
/// GIL-serialized: `worker_indy_refusal` aborts a gil-free writer before any of them runs,
/// which is why this cast is the right bridge rather than a shared-mutable table.
#[inline(always)]
#[allow(clippy::mut_from_ref, invalid_reference_casting)]
fn vm_mut(vm: &crate::vm::thread_context::SharedVm) -> &mut Vm {
    unsafe {
        &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm)
    }
}

/// The site's already-published `ConstantCallSite` object, if it has one. Both tables are
/// insert-once and never removed (`arena_indy_singletons` holds PINNED objects, whose addresses
/// are stable for the process), so a hit is a read plus a push and nothing else.
#[inline]
fn linked_singleton(vm: &Vm, site: (usize, u16)) -> Option<Value> {
    // ARENA twin FIRST, for the reason `worker_indy_refusal` records: under
    // RAVA_ARENA_LAMBDA_SINGLETON only this table is filled, and checking the handle table alone
    // declared every such site UNLINKED.
    #[cfg(feature = "next-gen-gc")]
    if let Some(&addr) = vm.arena_indy_singletons.get(&site) {
        if let Some(p) = crate::arena::ArenaPtr::new(addr as *mut u8) {
            return Some(Value::Obj(p));
        }
    }
    vm.indy_singletons.get(&site).map(|&h| Value::Ref(h))
}

/// `invokedynamic` — resolve/link the call site once per (thread, site), then mint.
///
/// ⭐**`&SharedVm`** — the indy family's tracer bullet, the same shape as the invoke* family
/// (`invokestatic`/`invokespecial`/`invokevirtual`): the function runs on the plain shared
/// reference (`&mut Vm` callers coerce for free), and the ONLY exclusivity left on the path
/// is the `vm_mut` leaves — the linkage `ldc`s here and the minting writes in
/// `invokedynamic_assemble` below. The GIL gate at the top reads `vm` through `&Vm` methods
/// only (`worker_indy_refusal` is a pure lookup), so a gil-free worker never reaches a
/// `vm_mut` leaf: the refusal aborts its region ahead of every shared write.
pub(crate) fn invokedynamic(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    index: u16,
) -> Result<(), VmError> {
    let site = (std::sync::Arc::as_ptr(&frame.class) as usize, index);
    // THE SAME REFUSAL THE COMPILED INDY TAKES (jit/callbacks/invoke.rs). Linking a call site
    // resolves bootstrap methods and publishes a singleton — shared state a GIL-free worker may
    // not touch. `worker_indy_refusal` already answers "is this site ALREADY linked, so that the
    // evaluation is a pure read?"; it was only ever consulted from compiled code, while workers
    // also reach this function whenever a fork body interprets an indy.
    //
    // FIRST, ahead of every cache probe: it may take the resolve lock for the whole call, and the
    // abort it can raise must happen before anything is read or pushed.
    #[cfg(feature = "next-gen-gc")]
    let _worker_gate = if crate::parallel::gilfree() {
        let refusal = crate::opcodes::worker_indy_refusal(vm, &frame.class, index);
        if crate::worker_locked_exec_enabled() {
            Some(crate::jit::resolve_lock_acquire())
        } else if refusal.is_some() {
            crate::parallel::request_abort();
            return Err(VmError::RegionAbort);
        } else {
            None // already linked: the evaluation is a read plus a push
        }
    } else {
        None
    };
    // WARM: this thread has linked this site. One probe answers the whole question — which plan,
    // which descriptor, how many operands, and whether the site can be a singleton at all.
    if fast_indy_enabled() {
        if let Some(link) = linked_get(site) {
            // NON-CAPTURING lambda singleton (JVM ConstantCallSite parity): the linked site
            // yields the SAME instance forever. A fresh allocation per call made every
            // identity-guarded lambda dispatch site megamorphic (each call minted a new identity
            // → PIC cap → general path on every SAM call) and churned an allocation + $fi store
            // per call. The handle is a GC root via `Vm::indy_singletons`.
            //
            // Gated on `link.singleton` because a CAPTURING site can never have an entry in
            // either table, and 99.8% of this traffic is capturing: two map probes per closure
            // that could only ever miss.
            if link.singleton {
                if let Some(v) = linked_singleton(vm, site) {
                    frame.push(v);
                    return Ok(());
                }
            }
            return invokedynamic_assemble(vm, frame, site, &link);
        }
    }
    // COLD IN THIS THREAD. The singleton probe stays AHEAD of the link here and must: another
    // thread (the main one) may already have published this site's object, and a worker that
    // linked it locally instead would fall into the mint arm and try to publish into
    // `Vm::indy_singletons` itself — the shared mutation `worker_indy_refusal` just cleared it
    // of, on the grounds that this read would serve it.
    if let Some(v) = linked_singleton(vm, site) {
        frame.push(v);
        return Ok(());
    }
    if let Some((plan, desc)) = crate::tlcache::indy_get(site) {
        let link = Rc::new(LinkedSite::link(plan, desc));
        if fast_indy_enabled() {
            linked_put(site, &link);
        }
        return invokedynamic_assemble(vm, frame, site, &link);
    }
    // Gather everything owned up front so the pool borrow ends before we mutate
    // the operand stack.
    let (bsm_owner, bsm_name, desc, plan) = {
        let pool = &frame.class.file.pool;
        let (bootstrap, indy_name, desc) = pool.invoke_dynamic(index).ok_or(VmError::BadConstant(index))?;
        let desc = desc.to_string();
        let indy_name = indy_name.to_string();
        let bm = frame
            .class
            .file
            .bootstrap_methods
            .get(bootstrap as usize)
            .ok_or(VmError::BadConstant(index))?
            .clone();
        let handle = pool.method_handle(bm.handle_index).ok_or(VmError::BadConstant(index))?;
        let (bsm_owner, bsm_name) = (handle.owner.to_string(), handle.name.to_string());

        let plan = match bsm_owner.as_str() {
            "java/lang/invoke/StringConcatFactory" => {
                if bsm_name == "makeConcatWithConstants" {
                    let ridx = *bm.args.first().ok_or(VmError::BadConstant(index))?;
                    let recipe = match vm_mut(vm).ldc(&frame.class, ridx)? {
                        Value::Str(s) => s.to_string(),
                        _ => return Err(VmError::BadConstant(ridx)),
                    };
                    let mut consts = Vec::new();
                    for &a in &bm.args[1..] {
                        consts.push(vm_mut(vm).ldc(&frame.class, a)?);
                    }
                    Plan::Concat { recipe: Some(recipe), consts }
                } else {
                    Plan::Concat { recipe: None, consts: Vec::new() }
                }
            }
            // metafactory / altMetafactory: static arg 1 is the impl MethodHandle.
            "java/lang/invoke/LambdaMetafactory" => {
                let mh = *bm.args.get(1).ok_or(VmError::BadConstant(index))?;
                let (kind, m) = pool.method_handle_full(mh).ok_or(VmError::BadConstant(mh))?;
                // Intern the call-site identity ONCE. `Heap::intern_class` dedups into the
                // class-name pool, so `owner` (usually a loaded class name) is shared with zero
                // extra allocation; name/descriptor dedup among themselves. The resolved Plan is
                // cached per site (`indy_put` + the linked table), so every later instantiation
                // clones this one Arc.
                let impl_site = std::sync::Arc::new(crate::LambdaSite::new(
                    vm.heap.intern_class(m.owner),
                    vm.heap.intern_class(m.name),
                    vm.heap.intern_class(m.descriptor),
                ));
                Plan::Lambda { kind, site: impl_site }
            }
            // Records: ObjectMethods.bootstrap synthesises equals/hashCode/toString.
            "java/lang/runtime/ObjectMethods" => {
                // args[0]=record Class, args[1]=";"-joined field names, rest getters.
                let names = match bm.args.get(1) {
                    Some(&i) => match vm_mut(vm).ldc(&frame.class, i)? { Value::Str(s) => s.to_string(), _ => String::new() },
                    None => String::new(),
                };
                // ⛔**THE NAME STRING IS NOT THE FIELD LIST, AND TRUSTING IT WAS AN ADAPTATION.**
                //
                // `args[1]` exists so `ObjectMethods.bootstrap` can render `toString()`; the JVM
                // reaches the VALUES through the getter MethodHandles in `args[2..]` and never
                // resolves a field by that text. An obfuscator therefore has no reason to rewrite
                // it — and Mojang's does not. Vanilla Minecraft's `amt$a` declares
                // `final amo a; final amo b;` while its bootstrap string is still
                // `"registry;identifier"`, so resolving components from the string asked for
                // `amt$a.registry`, got an UNQUALIFIED field id (the owner walk finds no such
                // field and breaks at `java/lang/Record`, which ships no bytecode here), and the
                // first arena read with that id panicked: "arena getfield of non-layout field
                // id=600 (registry) on class amt$a; layout ids=[598,599]".
                //
                // On a DEOBFUSCATED jar the string and the fields agree, which is exactly why this
                // never showed up: the engine was reading a coincidence of one input as a rule.
                //
                // ⭐**The record's own declared instance fields ARE the component list, in order.**
                // JLS 8.10.1: a record's components are declared, in the order written, as private
                // final instance fields of the record class. That is bytecode-derived, obfuscation
                // independent, and the same answer on both jars. The string is kept ONLY for the
                // rendered name, and only where it agrees in arity.
                let declared: Vec<(String, Box<str>)> = frame
                    .class
                    .file
                    .fields
                    .iter()
                    .filter(|m| m.access_flags & 0x0008 == 0) // ACC_STATIC — never a component
                    .map(|m| (m.name.to_string(), m.descriptor.as_str().into()))
                    .collect();
                let from_string: Vec<String> =
                    names.split(';').filter(|s| !s.is_empty()).map(String::from).collect();
                // Fall back to the string only when the class carries no declared instance fields
                // to read (a shape this engine has not seen, but the old behaviour is the safer
                // answer there than an empty component list).
                let (fields, descs): (Vec<String>, std::sync::Arc<[Box<str>]>) = if declared
                    .is_empty()
                {
                    let d: std::sync::Arc<[Box<str>]> =
                        from_string.iter().map(|_| Box::<str>::from("")).collect();
                    (from_string, d)
                } else {
                    (
                        declared.iter().map(|(n, _)| n.clone()).collect(),
                        declared.iter().map(|(_, d)| d.clone()).collect(),
                    )
                };
                Plan::Record { kind: indy_name.clone(), fields, descs }
            }
            // Java 21 pattern-matching switch. Static args are the case labels.
            "java/lang/runtime/SwitchBootstraps" => {
                let mut labels = Vec::with_capacity(bm.args.len());
                for &a in &bm.args {
                    labels.push(vm_mut(vm).ldc(&frame.class, a)?);
                }
                Plan::Switch { labels }
            }
            _ => return Err(VmError::UnsupportedNative(format!("invokedynamic {bsm_owner}.{bsm_name}"))),
        };
        (bsm_owner, bsm_name, desc, plan)
    };
    let _ = (bsm_owner, bsm_name);
    // Link the CallSite. `tlcache::indy_put` is kept alongside the linked table because a THIRD
    // party reads it: `tlcache::indy_warm_capturing_lambda` is the "shape 2" arm of
    // `worker_indy_refusal`, and a worker denied there aborts its region. One insert per
    // (thread, site) buys that predicate exactly the answer it had before.
    crate::tlcache::indy_put(site, plan.clone(), desc.clone());
    let link = Rc::new(LinkedSite::link(plan, desc));
    if fast_indy_enabled() {
        linked_put(site, &link);
    }
    invokedynamic_assemble(vm, frame, site, &link)
}

/// Mint from a LINKED call site — the per-execution part, and only that: pop the dynamic args and
/// assemble the result. Everything it reads out of `link` was fixed when the site was linked.
///
/// ⭐**`&SharedVm`** — same family shape as `invokedynamic` above: read-only probes
/// (`lambda_dedup.get`, `arena_lambda_dedup.get`, `heap.class_of`, `get_field_syn`) stay
/// direct, and the genuinely-`&mut` leaves — `value_to_string`, the `gc_protect` root set,
/// `arena_collect_jit`, `heap.alloc_lambda`, and every dedup/singleton table INSERT — go
/// through `vm_mut`. The table inserts are GIL-serialized shared state (`worker_indy_refusal`
/// aborts a gil-free writer ahead of this function), so the transitional cast is the right
/// bridge, not a per-table interior-mutability rewrite.
pub(crate) fn invokedynamic_assemble(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    site: (usize, u16),
    link: &LinkedSite,
) -> Result<(), VmError> {
    // WHICH indy plan the traffic is. `invokedynamic` totals ~5.7% of a boot's host profile
    // (`jit_indy` 2.61 + `invokedynamic_assemble` 1.91 + `invokedynamic` 0.48 + `tlcache::indy_get`
    // 0.68), the largest single LANGUAGE FEATURE in it — and `Plan::Concat` means that share may
    // be string concatenation (`"" + x` compiles to a StringConcatFactory indy on Java 9+) rather
    // than lambdas. Nothing had separated them. `RAVA_DIAG=indy`.
    match &link.plan {
        Plan::Concat { .. } => crate::diag::indy_concat().inc(),
        Plan::Lambda { .. } => {
            crate::diag::indy_lambda().inc();
            // A lambda that captures NOTHING is a singleton in Java: the call site binds one
            // object and every later execution returns it. One that captures must allocate.
            // The split decides whether a per-site cache can serve this traffic at all.
            // `link.argc`, not a fresh `count_args(desc)` — the descriptor walk ran per
            // execution to feed a counter that is disarmed unless `RAVA_DIAG` names it.
            if link.argc == 0 {
                crate::diag::indy_lambda_noncapturing().inc();
            }
        }
        Plan::Record { .. } => crate::diag::indy_record().inc(),
        Plan::Switch { .. } => crate::diag::indy_switch().inc(),
    }
    // The indy descriptor's arguments are the captured/dynamic values. `pop_args_n` with the
    // linked arity: `pop_args` would re-walk the descriptor to recover a number the link already
    // holds.
    // `mut` for the arena capturing arm alone, which ROOTS the captures by moving them into
    // `gc_protect` instead of cloning them in (see there). Every other arm only reads.
    #[allow(unused_mut)]
    let mut dyn_args = pop_args_n(vm, frame, link.argc)?;
    match &link.plan {
        Plan::Concat { recipe, consts } => {
            // Stringify each argument by its declared type — crucially via the
            // real toString() for objects (display() rendered them as
            // "<object@N>", so `"" + anyObject` was broken — the `+` operator
            // compiles to this StringConcatFactory indy on Java 9+).
            let types: &[u8] = &link.concat_types;
            let mut dyn_strs = Vec::with_capacity(dyn_args.len());
            for (i, v) in dyn_args.iter().enumerate() {
                dyn_strs.push(match types.get(i).copied() {
                    Some(b'Z') => if v.as_int().unwrap_or(0) != 0 { "true".to_string() } else { "false".to_string() },
                    Some(b'C') => char::from_u32(v.as_int().unwrap_or(0) as u32).map(String::from).unwrap_or_default(),
                    _ => vm_mut(vm).value_to_string(v).unwrap_or_else(|_| v.display()),
                });
            }
            let out = build_concat(recipe.as_deref(), &dyn_strs, consts);
            frame.push(Value::Str(out.into()));
        }
        Plan::Lambda { kind, site: impl_site } => {
            let kind = *kind;
            // Closure-minting stopwatch (RAII, inert unless RAVA_GC_STATS): times this
            // arm identically in BOTH representations so an A/B attributes a wall delta
            // to minting rather than inferring it.
            #[cfg(feature = "next-gen-gc")]
            let _mint_clock = crate::nextgen::lambda_birth_clock();
            let capturing = !dyn_args.is_empty();
            // M3 LAMBDA-DEDUP (gated): an arity-1 capturing site with an exact-keyable captured value
            // returns the EXISTING identical closure instead of allocating a fresh one. A hit is fully
            // built ($fi already set on that handle) → push + return. The cache is pruned at GC, and
            // between GCs no handle is swept, so a hit is always a live, correct closure.
            // GILFREE-RINGS: the rings lane does NOT dedup. `vm.lambda_dedup` is a
            // shared map, and a worker both reads and inserts here — the one
            // remaining shared mutation on the closure-minting arm. Skipping it is
            // strictly the weaker claim (a fresh closure per call is the pre-dedup
            // behaviour, and lambda identity is unspecified per JLS 15.27.4), and a
            // lane closure is a per-task transient that no other thread can reuse
            // anyway. Keyed on `in_lane` so the serial path and the pre-existing COW
            // forks keep deduping exactly as before.
            let dedup_key = if lambda_dedup_enabled()
                && dyn_args.len() == 1
                && !crate::gilfree_cf::in_lane()
            {
                lambda_capture_key(&dyn_args[0]).map(|(disc, val)| (site.0, site.1, disc, val))
            } else {
                None
            };
            // WIDE ARENA KEY (`RAVA_ARENA_LAMBDA_DEDUP`): the arity-1 key above covers 15.5%
            // of arena closure births (measured histogram: arity >= 6 is 61.9%, peaking at 7
            // and 8+). Keying on the WHOLE capture vector covers every arity, and bails only
            // on a capture kind `lambda_capture_key` cannot encode exactly — the same
            // exactness rule as the narrow key, so it can never alias `Ref(5)` with `Int(5)`.
            // Not gated on `in_lane` for the same reason the narrow key is: a lane closure is
            // a per-task transient no other thread can reuse.
            #[cfg(feature = "next-gen-gc")]
            let wide_key: Option<(usize, u16, Vec<(u8, i64)>)> =
                if crate::nextgen::arena_lambda_dedup_enabled()
                    && !dyn_args.is_empty()
                    && !crate::gilfree_cf::in_lane()
                {
                    let mut caps = Vec::with_capacity(dyn_args.len());
                    let mut ok = true;
                    for a in &dyn_args {
                        match lambda_capture_key(a) {
                            Some(kv) => caps.push(kv),
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    ok.then(|| (site.0, site.1, caps))
                } else {
                    None
                };
            // ARENA DEDUP (RAVA_ARENA_LAMBDA_DEDUP, default ON). Consulted BEFORE the handle
            // map because under RAVA_ARENA_LAMBDA this birth takes the arena path below, so a
            // handle hit could not be returned anyway. Same key, same JLS §15.27.4 licence
            // ("a new instance need not be created if an existing instance would suffice");
            // the entry is only ever read within the collection window that minted it, since
            // `arena_collect_jit` clears the map.
            #[cfg(feature = "next-gen-gc")]
            if let Some(k) = wide_key.as_ref() {
                if let Some(&addr) = vm.arena_lambda_dedup.get(k) {
                    if let Some(p) = crate::arena::ArenaPtr::new(addr as *mut u8) {
                        crate::nextgen::gc_stats_note_lambda(crate::nextgen::LambdaBirth::DedupHit);
                        frame.push(Value::Obj(p));
                        return Ok(());
                    }
                }
            }
            if let Some(k) = dedup_key {
                if let Some(&h) = vm.lambda_dedup.get(&k) {
                    #[cfg(feature = "next-gen-gc")]
                    crate::nextgen::gc_stats_note_lambda(crate::nextgen::LambdaBirth::DedupHit);
                    frame.push(Value::Ref(h));
                    return Ok(());
                }
            }
            // LAMBDA MIGRATION (RAVA_ARENA_LAMBDA): born a CAPTURING lambda
            // in the moving arena as a raw 0b00 Value::Obj. The capture kinds come from
            // the indy descriptor's STATIC parameter types (audit B1's key: a site's
            // capture signature is fixed) → a per-signature arena class whose ref_mask
            // traces reference captures + skips primitive ones. Non-capturing lambdas
            // stay per-site handle singletons (below) so the λ-PIC never sees an arena
            // lambda. NEVER collects between alloc and cell writes; captures are rooted
            // in gc_protect across the (rare) nursery-full collect + re-read forwarded.
            //
            // WORKER EXCLUSION: a gil-free pool worker runs on a pre-reserved DISJOINT
            // segment and must never drive a collection (`arena_collect_jit` below moves
            // the shared arena) nor bump the main TLAB. A worker keeps minting handle
            // lambdas — the coexistence path, always valid — so the parallel lane is
            // untouched by the migration.
            #[cfg(feature = "next-gen-gc")]
            if capturing && crate::nextgen::arena_lambda_enabled() && crate::gil::is_worker() {
                // Diagnostic (once): did the parallel lane actually reach an indy site?
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    if crate::env_flag!("RAVA_TRACE_ARENA_LAMBDA") {
                        eprintln!("[ARENA-LAMBDA] worker indy site — minting a HANDLE lambda (worker exclusion)");
                    }
                });
            }
            // NON-CAPTURING SINGLETON in the PINNED region (RAVA_ARENA_LAMBDA_SINGLETON).
            // With arena lambdas armed these are the flip's largest single residue —
            // 174,363 blocking cells over 4,063 objects — and they stay handles only because
            // the λ-PIC guards on a stable receiver address. A pinned arena object's address
            // is stable for the life of the process, so the guard is satisfied MORE strongly
            // here, not less. One object per call site, minted once and cached forever: no
            // capture cells to write, so the alloc is the whole birth.
            //
            // The `arena_indy_singletons` re-probe that used to open this block is gone: the
            // caller already answered it for every `link.singleton` site before reaching here.
            #[cfg(feature = "next-gen-gc")]
            if !capturing
                && crate::nextgen::arena_lambda_singleton_enabled()
                && crate::nextgen::arena_lambda_enabled()
                && !crate::gil::is_worker()
            {
                let shape = link.arena_shape(site, impl_site);
                if let Some(p) = crate::nextgen::pinned_alloc_lambda_shaped(&shape, kind) {
                    if shape.fi_cell != 0 {
                        unsafe {
                            crate::nextgen::set_cell(
                                p.as_ptr(),
                                crate::nextgen::ARENA_LAMBDA_FI_SLOT,
                                shape.fi_cell,
                            );
                        }
                    }
                    crate::nextgen::gc_stats_note_lambda(crate::nextgen::LambdaBirth::ArenaPinned);
                    vm_mut(vm).arena_indy_singletons.insert(site, p.addr() as usize);
                    frame.push(Value::Obj(p));
                    return Ok(());
                }
                // Pinned region exhausted — fall through to the handle path below, which is
                // always correct. A singleton table is bounded by the site count, so this is
                // a safety net rather than an expected path.
            }
            #[cfg(feature = "next-gen-gc")]
            if capturing && crate::nextgen::arena_lambda_enabled() && !crate::gil::is_worker() {
                // The closure's ARENA SHAPE is a function of the CALL SITE alone, so it
                // is resolved once and held on the linked entry (see `LinkedSite::arena_shape`).
                // Deriving it per instantiation cost a measured 1.26 s of a 2.79 s minting
                // budget on one ChunkFill run, and leaked one permanent box-table entry per
                // closure.
                let shape = link.arena_shape(site, impl_site);
                // Publish the site's shape for the inline mint — see `jit::lambda_shape`. The
                // shape is a per-site constant but does not exist until the bootstrap has been
                // resolved, so it is recorded by the very call the emitted mint replaces.
                //
                // ONCE per (thread, site). `record` is first-writer-wins on an atomic row, so
                // every later execution was hashing the site, packing the capture nibbles and
                // issuing a `compare_exchange` that could only fail — 2.5 M times a datapack run
                // to re-publish a constant. The flag is on the linked entry, so the second
                // execution skips the whole call.
                #[cfg(feature = "next-gen-gc")]
                if crate::jit::lambda_shape::enabled() && !link.shape_published.get() {
                    link.shape_published.set(true);
                    crate::jit::lambda_shape::record(
                        site.0,
                        site.1,
                        shape.cid,
                        shape.site_idx,
                        kind,
                        shape.fi_cell,
                        &shape.cap_kinds,
                    );
                }
                let base = crate::vm::thread_context::root_len(&vm_mut(vm).gc_protect);
                // ⭐**ROOTED BY MOVE, THEN READ BY BORROW — TWO `Value` CLONES PER CAPTURE GONE.**
                //
                // This arm OWNS `dyn_args` (`pop_args_n` built it for this execution) and, being
                // the last statement before its own `return`, never reads it again. It pushed a
                // CLONE of every capture into `gc_protect` and then cloned each one back OUT to
                // encode it — two clone+drop pairs per capture for values that were already ours.
                // `RAVA_DIAG=indy` counts **2,537,018 LambdaMetafactory assemblies in one datapack
                // run, 2,530,830 of them capturing**, and the measured capture-arity histogram
                // peaks at 7-8 with 61.9% at arity >= 6 — so this is on the order of fifteen
                // million redundant clone+drop pairs a run, each a tag test plus a 16-byte copy,
                // plus an `Arc` refcount round trip for a `Str` capture.
                //     was: 2 Value clone+drop per capture per capturing arena birth; is: 0.
                //
                // ⚠**THE GC CONTRACT IS UNCHANGED, AND IS THE REASON THE DUPLICATE WAS DEAD.**
                // `gc_protect` is the scanned root set; the plain Rust `Vec` `dyn_args` is not
                // scanned, which is exactly why the captures are pushed here before the alloc
                // loop that may collect — and it is why the encode below must read them back
                // from `gc_protect` (the FORWARDED copy) indexed off `base`, never from
                // `dyn_args`. Dropping the unscanned second copy removes a value that could only
                // ever have been STALE, over precisely the same rooting window as before.
                if crate::Vm::fast_indyfam_enabled() {
                    crate::vm::thread_context::append_roots(&mut vm_mut(vm).gc_protect, &mut dyn_args);
                } else {
                    for a in &dyn_args {
                        crate::vm::thread_context::push_root(&mut vm_mut(vm).gc_protect, a.clone());
                    }
                }
                let ncap = crate::vm::thread_context::root_len(&vm_mut(vm).gc_protect) - base;
                let p = loop {
                    if let Some(p) = crate::nextgen::tlab_alloc_lambda_shaped(&shape, kind) {
                        break p;
                    }
                    vm_mut(vm).arena_collect_jit();
                    if let Some(p) = crate::nextgen::tlab_alloc_lambda_shaped(&shape, kind) {
                        break p;
                    }
                    eprintln!("rava[next-gen-gc]: nursery full allocating arena-lambda — hard exit");
                    std::process::exit(3);
                };
                // `ncap`, not `dyn_args.len()`: with the move above `dyn_args` is empty, and the
                // count that matters is how many roots this arm pushed.
                for i in 0..ncap {
                    // BORROWED out of the root set, not cloned out of it. `encode_capture_cell`
                    // is a free function that cannot reach `vm`, so the borrow provably cannot
                    // span a mutation of `gc_protect` — borrowck refuses any version where it
                    // could — and the value it reads is the same forwarded one the clone read.
                    // A REFERENCE-typed capture slot (cap_kind >= 4) is traced by the GC
                    // ref_mask, so a raw primitive stored there would (a) be mis-decoded as a
                    // ref and (b) risk the GC misreading its bits — a boxed primitive captured
                    // as `Object` (Integer/Long/… ⇒ Value::Int/Long in the unboxed model) MUST
                    // be boxed (0b11 side-table) so decode recovers the exact Value and the GC
                    // skips it. `encode_arena_cell` already boxes Str/sentinels this way.
                    let cell = encode_capture_cell(
                        &crate::vm::thread_context::root_index(&vm.gc_protect, base + i),
                        shape.cap_kinds[i] >= 4,
                    );
                    unsafe {
                        crate::nextgen::set_cell(p.as_ptr(), crate::nextgen::AL_HDR_CELLS + i, cell);
                    }
                }
                crate::vm::thread_context::truncate_roots(&mut vm_mut(vm).gc_protect, base);
                if shape.fi_cell != 0 {
                    unsafe {
                        crate::nextgen::set_cell(
                            p.as_ptr(),
                            crate::nextgen::ARENA_LAMBDA_FI_SLOT,
                            shape.fi_cell,
                        );
                    }
                }
                // RAVA_TRACE_FICELL: the WRITE side. Printed for EVERY birth, including
                // `fi_cell == 0`, because "never written" and "written then lost" are
                // different bugs and the read side alone cannot tell them apart.
                if crate::env_flag!("RAVA_TRACE_FICELL") {
                    static NW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    if NW.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
                        eprintln!(
                            "[FICELL-WRITE] cid={} caps={} fi_cell={:#x}",
                            crate::nextgen::arena_class_id(p),
                            shape.cap_kinds.len(),
                            shape.fi_cell
                        );
                    }
                }
                crate::nextgen::gc_stats_note_lambda(if crate::nextgen::is_pinned(p.addr()) {
                    crate::nextgen::LambdaBirth::ArenaPinned
                } else {
                    crate::nextgen::LambdaBirth::ArenaMoving
                });
                // Publish for the arena dedup. Inserted only AFTER every capture cell and the
                // `$fi` cell are written, so a hit can never observe a half-built closure.
                if let Some(k) = wide_key {
                    vm_mut(vm).arena_lambda_dedup.insert(k, p.addr() as usize);
                }
                frame.push(Value::Obj(p));
                return Ok(());
            }
            #[cfg(feature = "next-gen-gc")]
            crate::nextgen::gc_stats_note_lambda(crate::nextgen::LambdaBirth::Handle);
            let handle = vm_mut(vm).heap.alloc_lambda(crate::Lambda { kind, site: impl_site.clone(), captured: dyn_args });
            // Record the functional-interface type (the indy descriptor's return
            // type) so a DEFAULT method declared on it — not the SAM — dispatches
            // to its real bytecode instead of being mistaken for the closure
            // (e.g. FloatModifier$Simple.argumentCodec() → Codec.FLOAT).
            //
            // `link.fi` is the same value the old `desc.rsplit(')')` + `fi.into()` built, minted
            // once at link time: this arm used to allocate a fresh `Arc<str>` for a per-site
            // CONSTANT on every closure.
            if let Some(fi) = &link.fi {
                vm.heap.put_field(handle, "$fi", fi.clone());
            }
            // Link a non-capturing lambda as a per-site singleton (see the check
            // at the top of `invokedynamic`): every later run of this site pushes
            // this same handle instead of allocating.
            if !capturing {
                crate::Vm::debug_assert_not_gilfree("indy_singletons (+ handles)");
                vm_mut(vm).indy_singletons.insert(site, handle);
                // Reverse index for the λ-PIC fill: membership proves the handle
                // is an immortal per-site singleton root (see the field docs).
                vm_mut(vm).indy_singleton_handles.insert(handle);
            } else if let Some(k) = dedup_key {
                vm_mut(vm).lambda_dedup.insert(k, handle);
            }
            frame.push(Value::Ref(handle));
        }
        Plan::Record { kind, fields, descs } => {
            let v = record_object_method(vm_mut(vm), site, kind, fields, descs, &dyn_args)?;
            frame.push(v);
        }
        Plan::Switch { labels } => {
            // dyn_args = [selector, startIndex].
            let target = dyn_args.first().cloned().unwrap_or(Value::Null);
            let start = dyn_args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let result = if matches!(target, Value::Null) {
                -1
            } else {
                let mut idx = labels.len() as i32;
                for (i, label) in labels.iter().enumerate().skip(start) {
                    if switch_label_matches(vm_mut(vm), &target, label) { idx = i as i32; break; }
                }
                if idx == labels.len() as i32 && crate::env_flag!("RAVA_TRACE_SWITCH") {
                    // An arena receiver prints as a bare pointer, which names nothing —
                    // and the whole point of this line is to identify the selector. Give
                    // it its arena class, whether it is a lambda, and its recorded
                    // functional interface, because "a lambda arrived where a record
                    // belongs" and "a record arrived with the wrong class" need opposite
                    // investigations.
                    let tc = match &target {
                        Value::Ref(h) => vm.heap.class_of(*h).unwrap_or("?").to_string(),
                        #[cfg(feature = "next-gen-gc")]
                        Value::Obj(p) => {
                            let cid = crate::nextgen::arena_class_id(*p);
                            let cn = crate::nextgen::class_name(cid)
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "?".to_string());
                            let lam = crate::nextgen::is_arena_lambda(*p);
                            let fi = if lam {
                                match arena_lambda_fi(*p) {
                                    Value::Str(s) => s.to_string(),
                                    _ => "<no $fi>".to_string(),
                                }
                            } else {
                                "-".to_string()
                            };
                            format!("arena cid={cid} class={cn} lambda={lam} fi={fi}")
                        }
                        v => format!("{v:?}"),
                    };
                    let lns: Vec<String> = labels.iter().map(|l| match l {
                        Value::Ref(h) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Name) { Value::Str(s) => s.to_string(), _ => format!("ref:{}", vm.heap.class_of(*h).unwrap_or("?")) },
                        v => format!("{v:?}"),
                    }).collect();
                    eprintln!("[SWITCH-NOMATCH] selector={tc} labels={lns:?}");
                }
                idx
            };
            frame.push(Value::Int(result));
        }
    }
    Ok(())
}
