//! Allocating in the arena: sites, constructor inlining, concat shapes.

use crate::*;

/// ⭐**THE ARENA DECISION IS A CONSTANT OF THE CLASS, AND IT WAS RE-DERIVED PER OBJECT.**
///
/// Kill switch for everything this file caches — the `new`-site route memo and the two env-var
/// reads below. `RAVA_NO_FAST_ARENAALLOC=1` restores the previous behaviour EXACTLY: every
/// question asked again, on every site and every allocation. Also off under `RAVA_NO_EMIT_MORE`.
///
/// ⚠**It is also off while `RAVA_ENUM_BIRTH_CENSUS` is armed.** That census emits one `note` per
/// VISIT to `arena_new_site`, so a memo that answers without visiting would silently shrink the
/// population the census exists to count — and that function's own comment records having been
/// bitten by exactly this once ("no readings" read as "the compiled path never mints enums", a
/// conclusion the probe could not support). A diagnostic that disagrees with itself between
/// builds is worse than a slow one.
#[cfg(feature = "next-gen-gc")]
fn fast_arenaalloc() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_ARENAALLOC").is_none()
            && !crate::flags::emit_more_disabled()
            && !crate::enum_birth::enabled()
    })
}

/// `RAVA_NO_ARENA_INLINE`, read ONCE per process.
///
/// WAS: `std::env::var(..)` — an environment-block scan plus a `String` allocation — at the head
/// of each of the FOUR plan builders, i.e. four per method compiled, and methods compiled is what
/// grows (≈1,500 on a product boot). IS: O(1). Semantics preserved to the letter, `var().is_ok()`
/// and not `var_os()`, so a non-UTF-8 value still fails to arm it.
#[cfg(feature = "next-gen-gc")]
fn arena_inline_disabled() -> bool {
    if !fast_arenaalloc() {
        return std::env::var("RAVA_NO_ARENA_INLINE").is_ok();
    }
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("RAVA_NO_ARENA_INLINE").is_ok())
}

/// `RAVA_NO_ARENA_CTORREF`, read ONCE per process — and this one sat on the PER-OBJECT path.
/// `alloc_ctor_receiver` runs for every `Foo::new` constructor-reference invocation (all 29,671
/// block states among them, per its own doc comment), and each paid an environment lookup before
/// it was allowed to ask anything about the class. WAS O(environment) per OBJECT, IS O(1).
#[cfg(feature = "next-gen-gc")]
fn ctorref_disabled() -> bool {
    if !fast_arenaalloc() {
        return std::env::var_os("RAVA_NO_ARENA_CTORREF").is_some();
    }
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_ARENA_CTORREF").is_some())
}

/// A `new`-site verdict SPLIT BY WHETHER IT CAN EVER CHANGE — the same discipline
/// `opcodes::NewRoute` and `try_arena_alloc_by_name` already apply, which is why the split lives
/// in the type and not in a comment: only a PERMANENT arm may be remembered.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
pub(crate) enum SiteRoute {
    /// PERMANENT handle. Native-backed by namespace (a pure function of the name and a
    /// process-constant `OnceLock`), enum-shaped with the enum flag disarmed (a pure function of
    /// the loaded class file and that same kind of flag), or a native-backed ancestor — which
    /// `birth_native_backed_ancestor` ALREADY freezes in its own memo, so nothing new is frozen
    /// here. None of the three un-decides.
    Handle,
    /// PERMANENT arena. `<clinit>` has run (monotone), the super chain is COMPLETE — a complete
    /// layout is inserted into `class_layout_cache` exactly once and never recomputed, because
    /// `class_field_layout` returns early on a cache hit — and `register_class` is id-stable
    /// (`nextgen::tests::register_class_is_stable`). So `(cid, nf)` is a class constant.
    Arena { cid: u32, nf: u32 },
    /// NEVER remembered. The class will not load YET, `<clinit>` has not run yet, or the layout is
    /// still PARTIAL — an incomplete chain completes the moment the missing super loads, and this
    /// verdict must be allowed to change with it. `partial` carries whatever `arena_new_site`
    /// returned before this split, so that function's contract is bit-identical.
    Unstable { partial: Option<(u32, u32)> },
}

impl Vm {
    /// Build an exception object of `class` carrying `message` (for natives /
    /// VM-raised throwables like ArithmeticException).
    /// JVM field identity: a getfield/putfield names (owner, name), but the
    /// field's STORAGE identity is its DECLARING class — a subclass redeclaring
    /// the same name SHADOWS it (two distinct fields; rampant in obfuscated
    /// jars, e.g. 1.12's BlockState.a String vs BlockStateEnum.a Map). Walk the
    /// owner's super chain to the first class declaring an INSTANCE field of
    /// that name and intern "Declaring#name". Unresolvable owners / undeclared
    /// names (synthetic $-fields, native-made fields) keep the bare name.
    /// Per-class instance-field layout: the ordered field-identity ids a fully
    /// populated object of `class` would hold — superclass fields first, then
    /// this class's own fields in declaration order (matching the JVM's layout
    /// order). Each id is the SAME "Decl#name" identity `resolve_field_id`
    /// produces, so the layout slot of a field equals what every access resolves
    /// to. Cached per class. Lever-#1 foundation; see `class_layout_cache`.
    /// INLINE TLAB BUMP (next-gen-gc): walk a method's bytecode for `new` (0xbb)
    /// sites and resolve each whose class is arena-eligible AND already initialised
    /// into `(class_id, nfields)`, so compiled code bump-allocates it inline with no
    /// callback. Two passes to decouple the immutable pool borrow from the `&mut
    /// self` class resolution. Empty when arena inlining is opted out.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn build_new_arena(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
    ) -> std::collections::HashMap<u16, (u32, u32)> {
        let mut map = std::collections::HashMap::new();
        if arena_inline_disabled() {
            return map;
        }
        // Pass 1: collect (cp index, class name) for every `new`, no `&mut self`.
        //
        // The names are BORROWED from the pool, not copied. The two-pass split exists to keep the
        // immutable pool borrow off the `&mut self` resolution, and it never needed an owned
        // `String` to do that: `class` is a separate `Arc<LoadedClass>` from `self` — provably, or
        // `self.build_new_arena(code, class)` would not compile at all — so a `&str` into its pool
        // outlives pass 2 without borrowing the VM. WAS one heap allocation + copy per `new` SITE
        // per method compiled; IS zero. (Same edit in the two builders below.)
        let bc = &code.bytecode;
        let pool = &class.file.pool;
        let mut sites: Vec<(u16, &str)> = Vec::new();
        let mut pc = 0;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = jit::insn_len_at(bc, pc) else { break };
            if op == 0xbb {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let Some(n) = pool.class_name(idx) {
                    sites.push((idx, n));
                }
            }
            pc += len;
        }
        // Diagnostic-only: report resolved plans, never use observations for admission.
        static TRACE_NEW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let trace_new = *TRACE_NEW.get_or_init(||
            std::env::var_os("RAVA_TRACE_NEW_PLANS").is_some());
        // Pass 2: resolve eligibility (needs `&mut self`).
        for (idx, name) in sites {
            let route = self.arena_new_site_route(name);
            if trace_new {
                let method = class.file.methods.iter().find(|m|
                    m.code.as_ref().is_some_and(|c| std::ptr::eq(c, code)));
                if let Some(method) = method {
                    let verdict = match route {
                        SiteRoute::Arena { .. } => "arena",
                        SiteRoute::Handle => "handle",
                        SiteRoute::Unstable { partial: None } => "unstable",
                        SiteRoute::Unstable { partial: Some(_) } => "partial",
                    };
                    eprintln!("[NEW-PLAN] {}.{}{} cp={} target={} route={}",
                        class.file.this_class, method.name, method.descriptor, idx, name, verdict);
                }
            }
            let pair = match route {
                SiteRoute::Arena { cid, nf } => Some((cid, nf)),
                SiteRoute::Handle => None,
                SiteRoute::Unstable { partial } => partial,
            };
            if let Some(v) = pair {
                map.insert(idx, v);
            }
        }
        map
    }

    /// Resolve a `new` target class to `(class_id, nfields)` IFF it is arena-eligible
    /// (a pure bytecode class — not native-backed, not an enum) AND already
    /// initialised. Mirrors `opcodes::new_object_value`'s arena predicate, plus the
    /// initialised guard: the inline path skips the callback's `<clinit>` trigger, so
    /// an un-initialised class must stay on the callback. A hot allocating loop has
    /// long since run the class's `<clinit>`, so eligibility is the common case.
    #[cfg(feature = "next-gen-gc")]
/// `RAVA_PARTIAL_ARENA_CID=1` restores the pre-fix behaviour: mint the arena class id from a
/// PARTIAL layout when the super chain is transiently unloadable. Kept as the A/B arm — the fix
/// changes which objects are arena-eligible on a first visit, so "identical" has to be shown, not
/// assumed.
fn partial_arena_cid_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PARTIAL_ARENA_CID").is_some())
}

    pub(crate) fn arena_new_site(&mut self, name: &str) -> Option<(u32, u32)> {
        match self.arena_new_site_route(name) {
            SiteRoute::Handle => None,
            SiteRoute::Arena { cid, nf } => Some((cid, nf)),
            SiteRoute::Unstable { partial } => partial,
        }
    }

    /// The same verdict, MEMOISED BY CLASS NAME and classified by permanence.
    ///
    /// ⭐**WHAT GREW: objects allocated, and sites × methods compiled.** This predicate is a
    /// constant of the class — one name is one class identity for the process lifetime — and it
    /// was recomputed from scratch at every asking. Each asking cost, in order: up to seven
    /// substring searches (`birth_native_backed_alloc`), a loader hash + `Arc` clone
    /// (`get_or_load`), a thread-local hash (`birth_native_backed_ancestor`), an interner hash +
    /// `Arc` clone (`intern_class`), an `RwLock` read + hash + `Arc` clone (`class_field_layout`),
    /// and then `register_class`, which takes TWO global mutexes (the registry and its name index)
    /// to hand back an id it has already handed back. IS: one thread-local hash.
    ///
    /// This is not the shape the round warned about — a memo losing to a two-entry scan the caller
    /// already had in a register. There is no scan here to lose to: the memo REPLACES five hash
    /// tables keyed on the same string, one reader-writer lock and two mutexes.
    ///
    /// SIZE: one `(Box<str>, SiteRoute)` per distinct class name that reaches a `new` /
    /// `invokespecial <init>` / ctor-ref site — about 60 B with the name's own allocation and
    /// hashbrown's load factor. A product boot registers ~1,590 arena classes; at a generous
    /// 4,000 entries that is ~240 KB, per thread that compiles or allocates, grown lazily and
    /// touched once per class thereafter.
    ///
    /// THREAD-LOCAL, like `birth_native_backed_ancestor`'s memo and `try_arena_alloc_by_name`'s,
    /// and for the same reason: the JIT worker asks these questions too, and a shared table would
    /// need a lock to answer what a hash answers.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn arena_new_site_route(&mut self, name: &str) -> SiteRoute {
        thread_local! {
            static ROUTES: std::cell::RefCell<FastMap<Box<str>, SiteRoute>> =
                std::cell::RefCell::new(FastMap::default());
        }
        let fast = fast_arenaalloc();
        if fast {
            if let Some(r) = ROUTES.with(|m| m.borrow().get(name).copied()) {
                return r;
            }
        }
        let r = self.arena_new_site_uncached(name);
        // UNSTABLE IS NEVER INSERTED — a class that will not load yet may load later, an
        // un-initialised class initialises, and a partial super chain completes. Freezing any of
        // those would force the class to a heap handle for the process lifetime, which is the
        // failure mode `try_arena_alloc_by_name` documents and avoids the same way.
        if fast && !matches!(r, SiteRoute::Unstable { .. }) {
            ROUTES.with(|m| {
                m.borrow_mut().insert(name.into(), r);
            });
        }
        r
    }

    #[cfg(feature = "next-gen-gc")]
    fn arena_new_site_uncached(&mut self, name: &str) -> SiteRoute {
        // BIRTH NAMESPACE GUARD (RAVA_ARENA_BIRTH_NS_GUARD, default OFF) — must
        // mirror `new_object_value` exactly, or the JIT's inline `new` and the
        // callback would disagree about an object's kind. See
        // `opcodes::birth_native_backed_alloc`.
        // Census BEFORE the early returns. It used to sit after them, so a class rejected here
        // (or one whose load failed) was invisible - and "no readings" was then read as "the
        // compiled path never mints enums", which is a conclusion the probe could not support.
        //
        // Asked ONCE. The census arm below used to ask it too, so with `RAVA_ENUM_BIRTH_CENSUS`
        // armed the whole substring gauntlet (`is_collection` / `is_native_map` / `is_entry_type`
        // / `is_native_io_class`) ran twice over the same name for the same answer.
        // Prefer class-load storage facts for classes already resolved by the loader. A cold or
        // uncovered class remains on the legacy conservative gate until its provider contract is
        // complete; this bridge does not infer absence from `Unknown`.
        let native_state = self.loader.get_loaded(name).map(|class| class.native_state);
        let native_backed = native_state
            .and_then(crate::native_state::NativeStateFact::slots)
            .is_some()
            || crate::opcodes::birth_native_backed_alloc(name);
        if crate::enum_birth::enabled() && native_backed {
            crate::enum_birth::note("jit-native-backed", name, false, true, false);
        }
        if native_backed {
            return SiteRoute::Handle;
        }
        let Ok(loaded) = self.loader.get_or_load(name) else {
            return SiteRoute::Unstable { partial: None };
        };
        // MIRROR `new_object_value` (opcodes.rs:2503): under RAVA_ARENA_ENUM an enum is
        // arena-born there, and this JIT-side twin used to reject it unconditionally —
        // so with that flag ON the two birth paths DISAGREED about an object's kind
        // (exactly what the comment above forbids), and the flag could never reach full
        // coverage because every enum minted through a compiled `new` stayed a handle.
        let loaded_is_enum_shaped = loaded.file.access_flags & 0x4000 != 0
            || loaded.file.super_class.as_deref() == Some("java/lang/Enum");
        let is_enum = !crate::nextgen::arena_enum_enabled() && loaded_is_enum_shaped;
        // A subclass of a native-backed JDK type (entry/collection/map) inherits
        // synthetic heap-only fields, so it must be a handle — mirror
        // new_object_value, else the JIT inline-news it as an arena object and its
        // native accessor (e.g. ImmutableMapEntry.getKey) no-ops → caller underflow.
        let initialized = loaded.initialized.load(std::sync::atomic::Ordering::Relaxed);
        drop(loaded);
        let native_super =
            self.birth_native_backed_ancestor(name) && !self.overrides_collection_backing(name);
        // Which term forces the handle for an enum-shaped class (RAVA_ENUM_BIRTH_CENSUS, inert
        // otherwise). `looks_enum` is computed independently of the flag so the tally is the same
        // population whether the migration is armed or not.
        if crate::enum_birth::enabled() {
            let looks_enum = loaded_is_enum_shaped;
            if looks_enum {
                crate::enum_birth::note("jit", name, is_enum, native_super, !initialized);
            }
        }
        // PERMANENT vs TRANSIENT, and the order matters: `is_enum` and `native_super` are class
        // constants, `initialized` is a one-way latch that has simply not flipped YET.
        if is_enum || native_super {
            return SiteRoute::Handle;
        }
        if !initialized {
            return SiteRoute::Unstable { partial: None };
        }
        let arc = self.heap.intern_class(name);
        let layout = self.class_field_layout(&arc);
        // ⛔**REGISTERING BEFORE THE COMPLETENESS TEST POISONS THE CLASS ID FOREVER.**
        //
        // `nextgen::register_class` is FIRST-WINS: on a name hit it returns the existing id and
        // DISCARDS the layout it was handed (`nextgen/classes.rs`), and the per-thread
        // `ClassMirror` built from that record is then cached on the explicit claim that "a class
        // id, once observed, names the same immutable record forever". Nothing in the tree ever
        // rewrites an `ArenaClass.layout`.
        //
        // So a single visit to a `new` site while the class's super was TRANSIENTLY unloadable —
        // re-entrancy, or mid-`<clinit>` on another thread — minted the id against a PARTIAL
        // layout, and every later birth of that class, including the ones that correctly got the
        // complete layout and the right cell count, kept answering `slot_of` from the frozen short
        // record. The object exists with enough cells and the registry insists it has fewer.
        //
        // That is the vanilla-Minecraft crash: `arena getfield of non-layout field id=600
        // (registry) on class amt$a; layout ids=[598,599] names=[amt$a#a, amt$a#b]` — a registry
        // record holding only the class's OWN two declared fields, the inherited `registry`
        // missing, on a jar whose Mojang-library supers keep their real names and load fine.
        //
        // The completeness test therefore runs FIRST, and an incomplete chain mints nothing.
        // `class_field_layout` publishes a COMPLETE layout into this cache and returns a partial
        // one uncached, so presence here is exactly "the chain reached its root".
        let complete = self.runtime_shared().class_layout_contains(name);
        if !complete && !Self::partial_arena_cid_enabled() {
            // No id minted, so nothing to freeze; the caller falls to a heap handle and the NEXT
            // visit — after the super loads — mints the id against the complete layout. This also
            // closes the JIT hole: `arena_new_site` flattens `Unstable { partial }` into its
            // return value, and `jit/emit/ops/objects.rs`'s inline TLAB bump for `new` trusts that
            // pair with no completeness check of its own.
            return SiteRoute::Unstable { partial: None };
        }
        let cid = crate::nextgen::register_class(&arc, &layout);
        let nf = layout.len() as u32;
        if complete {
            SiteRoute::Arena { cid, nf }
        } else {
            SiteRoute::Unstable { partial: Some((cid, nf)) }
        }
    }

    /// Allocate a constructor-reference (`Foo::new`, REF_newInvokeSpecial) receiver on the
    /// ARENA (`Value::Obj`) when the class is arena-eligible — mirroring `new_object_value` /
    /// `arena_new_site` — else a heap handle (`Value::Ref`). The `new BlockState(...)` used to
    /// build every block state goes through a ctor-ref in StateDefinition's stream, NOT the `new`
    /// bytecode, so without this all 29671 states (and StateHolder subclasses) land on the heap.
    /// Caller MUST have run `<clinit>` first (the `initialized` guard) and MUST root the returned
    /// value across the following `<init>` (an arena ptr moves under GC).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn alloc_ctor_receiver(&mut self, name: &str) -> Value {
        if !ctorref_disabled() {
            // `SiteRoute::Arena` IS the old pair of tests, folded: `arena_new_site` returned
            // `Some((cid, nf))` AND `class_layout_cache` held a COMPLETE layout for the class.
            // Committing an arena object on a partial layout would panic on the first putfield to
            // a missing inherited cell, so the other two arms fall to a handle exactly as before —
            // `Unstable { partial: Some(..) }` is precisely the case where the old code got a
            // `Some` and then found the cache empty. On a memo hit this whole route — the loader
            // probe, the ancestor walk, the interner, the layout `RwLock` and `register_class`'s
            // two mutexes — collapses to one hash, PER OBJECT BORN THROUGH A CONSTRUCTOR REF.
            if let SiteRoute::Arena { cid, nf } = self.arena_new_site_route(name) {
                if let Some(p) = crate::nextgen::tlab_try_alloc(cid, nf) {
                    return Value::Obj(p);
                }
                self.arena_collect_jit();
                if let Some(p) = crate::nextgen::tlab_try_alloc(cid, nf) {
                    return Value::Obj(p);
                }
            }
        }
        Value::Ref(self.heap.alloc(name))
    }

    /// CONSTRUCTOR INLINER (next-gen-gc): walk a method's bytecode for
    /// `invokespecial <init>` sites whose receiver class is arena-eligible AND whose
    /// constructor is TRIVIAL (pure field setter, `jit::analyze_trivial_ctor`), and
    /// resolve each into an inline store plan (arg → field slot). Compiled code then
    /// stores the constructor args straight into the arena object's cells with no
    /// `rava_invoke` callback. Empty when arena inlining is opted out.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn build_ctor_inlines(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
    ) -> std::collections::HashMap<u16, jit::CtorInline> {
        let mut map = std::collections::HashMap::new();
        if arena_inline_disabled() {
            return map;
        }
        // Pass 1: collect (cp index, receiver class, <init> descriptor), no `&mut self`.
        // Borrowed from the pool, not copied — see `build_new_arena`. WAS TWO heap allocations
        // per `invokespecial <init>` site per method compiled, both only ever compared or used as
        // a lookup key; IS zero.
        let bc = &code.bytecode;
        let pool = &class.file.pool;
        let mut sites: Vec<(u16, &str, &str)> = Vec::new();
        let mut pc = 0;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = jit::insn_len_at(bc, pc) else { break };
            if op == 0xb7 {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let Some(m) = pool.member_ref(idx) {
                    if m.name == "<init>" {
                        sites.push((idx, m.owner, m.descriptor));
                    }
                }
            }
            pc += len;
        }
        // Pass 2: eligibility + trivial analysis + field-slot resolution.
        for (idx, owner, desc) in sites {
            // TRANSITIVELY-EMPTY ()V chain (`aload_0; invokespecial super.<init>()V;
            // return` at every level, terminating at Object.<init>): semantically
            // NOTHING for ANY receiver kind (no field writes anywhere), so the site
            // inlines as a zero-store plan — pop the receiver, emit nothing. This makes
            // a trivial abstract-super chain (e.g. fastutil AbstractDoubleCollection/
            // AbstractDoubleList.<init>, millions of boot calls) compile + run with zero
            // callbacks, independent of the order the chain's methods reach the JIT
            // threshold (a failed compile is cached, so order-dependence is fatal).
            if desc == "()V" && self.is_empty_ctor_chain(owner, 0) {
                map.insert(idx, jit::CtorInline { stores: Vec::new(), n_params: 0 });
                continue;
            }
            // Receiver must be an arena-eligible, initialised class (same predicate
            // as `new`) — else the object isn't an arena object with inline cells.
            if self.arena_new_site(owner).is_none() {
                continue;
            }
            // Analyze the <init> bytecode against the OWNER's pool; collect owned
            // store refs so the owner-class borrow ends before the `&mut self` below.
            let stores_raw = {
                let Ok(loaded) = self.loader.get_or_load(owner) else { continue };
                let Some(m) = loaded.file.method("<init>", desc) else { continue };
                let Some(ccode) = m.code.as_ref() else { continue };
                match jit::analyze_trivial_ctor(
                    &ccode.bytecode,
                    &loaded.file.pool,
                    desc,
                    !ccode.exceptions.is_empty(),
                ) {
                    Some(s) => s,
                    None => continue,
                }
            };
            // Resolve each store's field id + layout slot in the receiver class.
            // The layout is the RECEIVER's and does not move between stores, but it was fetched
            // INSIDE the loop — an `RwLock` read, a hash over the class name and an `Arc` clone
            // per STORE, which is fields-written-by-this-constructor. Hoisted: was O(stores) of
            // those per site, is 1. (The `position` scan stays: it walks the class's own field
            // slots, an array the caller already holds, and the round's own measurement says a
            // table loses to a short scan over data in hand.)
            let layout = self.class_field_layout(owner);
            let mut stores = Vec::with_capacity(stores_raw.len());
            let mut ok = true;
            for st in &stores_raw {
                let id = self.resolve_field_id(&st.field_owner, &st.field_name);
                match layout.iter().position(|&(i, _)| i == id) {
                    Some(s) => stores.push((st.arg_index, s as u32, id, st.jt)),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && !stores.is_empty() {
                let n_params = crate::count_args(desc);
                map.insert(idx, jit::CtorInline { stores, n_params });
            }
        }
        map
    }

    /// Is `owner`'s `<init>()V` a TRANSITIVELY-EMPTY constructor — the body exactly
    /// `aload_0; invokespecial super.<init>()V; return` at every level, no exception
    /// handlers, terminating at `java/lang/Object.<init>()V` (empty by JVM spec)?
    /// Such a chain writes nothing and observes nothing: executing it is identical to
    /// executing nothing, for any receiver kind. (`<init>` is never inherited, so the
    /// invokespecial owner IS the exact declaring class — no resolution ambiguity.)
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn is_empty_ctor_chain(&mut self, owner: &str, depth: u8) -> bool {
        // Known-EMPTY JDK root constructors (JDK spec/source: literally `{}`, no field
        // writes). These classes usually aren't loadable as bytecode here (outside the
        // jar), and the interpreter no-ops their unresolvable `<init>` anyway — so a
        // chain terminating in one is exactly as empty as one ending at Object. This is
        // what lets the fastutil Abstract*Collection ctor chains (2.9M boot calls each,
        // super = java/util/AbstractCollection) compile instead of bailing forever.
        if matches!(
            owner,
            "java/lang/Object"
                | "java/lang/Record"
                | "java/lang/Number"
                | "java/util/AbstractCollection"
                | "java/util/AbstractList"
                | "java/util/AbstractSet"
                | "java/util/AbstractMap"
        ) {
            return true;
        }
        if depth >= 8 {
            return false; // defensive: no legitimate empty chain is this deep
        }
        let Ok(loaded) = self.loader.get_or_load(owner) else { return false };
        let Some(m) = loaded.file.method("<init>", "()V") else { return false };
        let Some(code) = m.code.as_ref() else { return false };
        if !code.exceptions.is_empty() {
            return false;
        }
        let bc = &code.bytecode;
        if bc.len() != 5 || bc[0] != 0x2a || bc[1] != 0xb7 || bc[4] != 0xb1 {
            return false; // aload_0 ; invokespecial ; return — nothing else
        }
        let sidx = ((bc[2] as u16) << 8) | bc[3] as u16;
        let Some(sm) = loaded.file.pool.member_ref(sidx) else { return false };
        if sm.name != "<init>" || sm.descriptor != "()V" {
            return false;
        }
        // `sm` borrows `loaded`, an Arc this frame OWNS — not anything reached through `self` —
        // so the recursive `&mut self` call needs no owned copy of the super's name. Was one
        // `String` per chain level per site; is none.
        self.is_empty_ctor_chain(sm.owner, depth + 1)
    }

    /// SYNERGY LAYER 1 (next-gen-gc): walk a method's `invokevirtual` sites and, for
    /// each call to a TRIVIAL GETTER (`aload_0; getfield F; Xreturn`), record cp index
    /// → `(field id, type)`. The data-parallel body emitter folds such a call to a
    /// direct field read; when the receiver is a scalar-replaced virtual object the
    /// read hits a Cranelift register, so getter + allocation both disappear. Pure
    /// loader/pool reads + field-id interning.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn build_getter_inlines(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
    ) -> std::collections::HashMap<u16, (u32, jit::JT)> {
        let mut map = std::collections::HashMap::new();
        if arena_inline_disabled() {
            return map;
        }
        let bc = &code.bytecode;
        let pool = &class.file.pool;
        // Borrowed from the pool, not copied — see `build_new_arena`. This is the widest of the
        // three: THREE heap allocations per no-arg `invokevirtual` SITE per method compiled, all
        // three only ever used as lookup keys. Now zero.
        let mut sites: Vec<(u16, &str, &str, &str)> = Vec::new(); // cp, owner, name, desc
        let mut pc = 0;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = jit::insn_len_at(bc, pc) else { break };
            if op == 0xb6 {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let Some(m) = pool.member_ref(idx) {
                    if m.descriptor.starts_with("()") {
                        sites.push((idx, m.owner, m.name, m.descriptor));
                    }
                }
            }
            pc += len;
        }
        for (idx, owner, name, desc) in sites {
            let (fowner, fname, jt) = {
                let Ok(loaded) = self.loader.get_or_load(owner) else { continue };
                let Some(m) = loaded.file.method(name, desc) else { continue };
                let Some(ccode) = m.code.as_ref() else { continue };
                if !ccode.exceptions.is_empty() {
                    continue;
                }
                match jit::analyze_trivial_getter(&ccode.bytecode, &loaded.file.pool) {
                    Some(x) => x,
                    None => continue,
                }
            };
            let field_id = self.resolve_field_id(&fowner, &fname);
            map.insert(idx, (field_id, jt));
        }
        map
    }

    /// INLINE CONCAT (next-gen-gc): resolve `invokedynamic` concat sites whose
    /// dynamic args are ALL `int` and whose recipe is ASCII into a `ConcatShape`
    /// (baked literal byte-runs + per-arg itoa positions). Compiled code then
    /// assembles the result string inline — itoa each int straight into a
    /// TLAB-bumped ArenaString — with NO `rava_indy` callback. Every other concat
    /// shape (object args, non-ASCII literals, `` consts, non-String return)
    /// is omitted and stays on the callback. Pure pool/bootstrap reads (no &mut).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn build_concat_shapes(
        &self,
        code: &Code,
        class: &Arc<LoadedClass>,
    ) -> std::collections::HashMap<u16, jit::ConcatShape> {
        let mut map = std::collections::HashMap::new();
        if arena_inline_disabled() {
            return map;
        }
        let bc = &code.bytecode;
        let pool = &class.file.pool;
        let mut pc = 0;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = jit::insn_len_at(bc, pc) else { break };
            if op == 0xba {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let Some(shape) = Self::concat_shape_at(pool, &class.file.bootstrap_methods, idx) {
                    map.insert(idx, shape);
                }
            }
            pc += len;
        }
        map
    }

    /// Resolve a single `invokedynamic` concat site to a `ConcatShape`, or `None`
    /// if it is not an all-int / ASCII / String-returning `makeConcat[WithConstants]`
    /// (those stay on the `rava_indy` callback). Pure pool + bootstrap reads.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn concat_shape_at(
        pool: &rava_classfile::ConstantPool,
        bootstrap_methods: &[rava_classfile::BootstrapMethod],
        idx: u16,
    ) -> Option<jit::ConcatShape> {
        let (bootstrap, _name, desc) = pool.invoke_dynamic(idx)?;
        // Return type must be String.
        if !desc.ends_with(")Ljava/lang/String;") {
            return None;
        }
        // Every dynamic parameter must be `int` (one slot each).
        let inner = desc.get(1..desc.find(')')?)?;
        if inner.is_empty() || !inner.bytes().all(|c| c == b'I') {
            return None;
        }
        let nargs = inner.len();
        let bm = bootstrap_methods.get(bootstrap as usize)?;
        let handle = pool.method_handle(bm.handle_index)?;
        if handle.owner != "java/lang/invoke/StringConcatFactory" {
            return None;
        }
        let mut segs: Vec<jit::ConcatSeg> = Vec::new();
        let mut lit: Vec<u8> = Vec::new();
        match handle.name {
            "makeConcat" => {
                // No recipe: all dynamic args concatenate in order.
                for i in 0..nargs {
                    segs.push(jit::ConcatSeg::IntArg(i));
                }
            }
            "makeConcatWithConstants" => {
                let ridx = *bm.args.first()?;
                let recipe = pool.string_const(ridx)?;
                // `` consts (extra static args) → unsupported (rare); bail.
                if bm.args.len() > 1 {
                    return None;
                }
                let mut di = 0usize;
                for ch in recipe.chars() {
                    match ch {
                        '\u{1}' => {
                            if !lit.is_empty() {
                                segs.push(jit::ConcatSeg::Lit(std::mem::take(&mut lit)));
                            }
                            segs.push(jit::ConcatSeg::IntArg(di));
                            di += 1;
                        }
                        '\u{2}' => return None, // constant placeholder — unsupported
                        c if (c as u32) < 0x80 => lit.push(c as u8),
                        _ => return None, // non-ASCII literal — bail (byte≠char length)
                    }
                }
                if di != nargs {
                    return None; // placeholder/arg count mismatch — bail to callback
                }
                if !lit.is_empty() {
                    segs.push(jit::ConcatSeg::Lit(lit));
                }
            }
            _ => return None,
        }
        Some(jit::ConcatShape { segs, nargs })
    }
}
