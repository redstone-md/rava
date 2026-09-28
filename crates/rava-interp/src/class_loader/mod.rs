//! Class loading + caching. Resolves binary names (`a/b/C`) to parsed classes
//! from two sources, checked in order: a directory of `.class` files and/or a
//! jar. Each loaded class carries its own static-field storage and an
//! "initialized" flag so `<clinit>` runs exactly once (the VM drives that).


use std::sync::atomic::AtomicBool;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use rava_classfile::ClassFile;
use rava_jar::Jar;

use crate::value::Value;

pub struct LoadedClass {
    pub file: ClassFile,
    /// Static fields, lazily populated by `<clinit>` / `putstatic`.
    ///
    /// FxHash, not the std default. Every `getstatic` consults this map by NAME, and the RIP
    /// profile of blocks A+B showed SipHashing those Strings at 1.9% of wall
    /// (`hash::random::write` + `BuildHasher::hash_one<RandomState, String>`) — the engine keys
    /// every other hot map with `FastMap` and this one was simply missed.
    ///
    /// `RwLock`, not `RefCell`: this was the LAST `!Sync` field of `LoadedClass`, and therefore
    /// the last compiler-verified blocker on moving Cranelift codegen off the serial critical
    /// path. The note that guarded it called a lock swap impossible here because "a `RefCell` of
    /// guest `Value`s is the GIL problem in miniature" — true of the SEMANTICS, but it was being
    /// read as a statement about the TYPES, and those are separate. `Value` is `Int | Long |
    /// Float | Double | Str(Arc<str>) | Ref(u32) | Array(u32) | Obj(ArenaPtr) | Null | StdOut |
    /// StdErr`: primitives, an `Arc<str>`, plain handles, and an `ArenaPtr` that already carries
    /// `unsafe impl Send + Sync`. So `Value` was always `Send + Sync` and this swap needs no new
    /// unsafe.
    ///
    /// What the swap does NOT settle is whether concurrent static reads/writes are CORRECT under
    /// the GIL-free lane. It is not claimed here. Under the GIL the behaviour is unchanged, and
    /// every guard but `forward_statics`'s lives for a single expression.
    pub statics: std::sync::RwLock<crate::FastMap<String, Value>>,
    /// True once `<clinit>` has started (set before running, to break cycles).
    /// `AtomicBool`, not `Cell`: `Cell` is `!Sync`, which makes `LoadedClass` `!Sync` and
    /// therefore `Arc<LoadedClass>` `!Send` — the compiler-verified blocker on moving Cranelift
    /// codegen off the serial critical path (see
    /// `jit::compile_inputs_are_send_except_the_recorded_blocker`). Relaxed ordering: these flags
    /// are latches read under the GIL, and a relaxed load compiles to a plain `mov` on x86, so
    /// this is representation-only.
    pub initialized: AtomicBool,
    /// True once `<clinit>` has actually COMPLETED. Distinct from `initialized`
    /// (which latches at START, to break cycles): under the trampoline,
    /// `ensure_init_tramp` defers the clinit body, so there is a window where
    /// `initialized==true` but the statics are still empty. The 3c-parity probe
    /// (RAVA_PROBE_CLINIT) flags a static read in that window — the exact signature
    /// of the stackful-only null-static divergence.
    pub clinit_done: AtomicBool,
    /// ⭐**The INITIALIZING THREAD (JVMS §5.5), 0 when nobody owns the class.** `initialized`
    /// latches at START so a class can re-enter its own `<clinit>`, which is right for one
    /// thread and catastrophic for two: a second thread saw the latch, concluded "already
    /// initialised" and read statics the owner had not assigned yet. Owning the initialization
    /// lets the same thread recurse (as the JVM requires) while ANY OTHER thread waits.
    /// Zero cost while single-threaded — nothing reads it unless a real thread exists.
    pub init_owner: std::sync::atomic::AtomicU64,
    /// True if `<clinit>` ran and THREW: the class is in the JVM's "erroneous"
    /// state, and every later initialization attempt must throw
    /// NoClassDefFoundError (JVM §5.5) instead of silently proceeding on
    /// half-initialized statics.
    pub init_failed: AtomicBool,
    /// Per-call-site cache of a member ref's (owner, name, descriptor), keyed by constant-pool
    /// index — turns ref_parts from three String allocations per getfield/putfield/invoke into a
    /// refcount bump.
    ///
    /// `RwLock`, not `RefCell`, for two reasons that have nothing to do with hashing:
    ///
    /// * a `RefCell` makes `LoadedClass` `!Sync`, which was a compiler-verified blocker on moving
    ///   Cranelift codegen off the serial critical path, and
    /// * it forced GIL-free workers to SKIP the cache entirely — a borrow flag is non-atomic, so
    ///   concurrent borrows race it — leaving every interpreted member-ref op in a worker to
    ///   rebuild all three `Arc<str>`s from the pool. They now share it.
    ///
    /// ⚠ It was briefly a lazily-built DENSE table of `OnceLock`s indexed straight by pool index,
    /// to buy a lock-free read as well. That is cheaper per read and much more expensive overall:
    /// the table spans the WHOLE pool for every class that reaches the interpreter, at ~16 B a
    /// slot, so thousands of classes pay tens of megabytes of allocation and zero-init for a
    /// handful of live entries. Sparse and locked beats dense and lock-free here; the read was
    /// never the cost.
    ///
    /// The value is `Arc<(..)>` so an insert moves one pointer rather than the 48-byte triple.
    /// Racing writers are harmless: the value is a pure function of the immutable constant pool,
    /// so every writer computes an identical triple.
    ///
    /// ⚠ SUPERSEDED 2026-08-12 by `site_slots`. The warning above still describes why the FIRST
    /// dense attempt lost — 16 B a slot for a whole pool — but its last clause ("the read was
    /// never the cost") was a claim about a landscape that has since moved: with the call path
    /// down to 177 ns, the PC profile puts `ref_site_ref` at 7.1% of the interpreted call, all of
    /// it the RwLock pair plus the hash. The replacement is 8 B a slot, not 16.
    pub(crate) native_state: crate::native_state::NativeStateFact,
    pub site_slots: std::sync::OnceLock<Box<[std::sync::atomic::AtomicPtr<SiteRef>]>>,
}

/// The site table owns its entries; a class that IS dropped must not leak them. Nothing in the
/// tree drops a `LoadedClass` today (the loader never evicts), which is exactly why this has to
/// be written now rather than when something starts to.
impl Drop for LoadedClass {
    fn drop(&mut self) {
        if let Some(slots) = self.site_slots.get() {
            for slot in slots.iter() {
                let p = slot.load(std::sync::atomic::Ordering::Relaxed);
                if !p.is_null() {
                    // SAFETY: every non-null slot holds a `Box::into_raw` from `build_site`, and
                    // `&mut self` says no reader can be looking at it.
                    drop(unsafe { Box::from_raw(p) });
                }
            }
        }
    }
}

impl LoadedClass {
    fn new(mut file: ClassFile) -> Arc<Self> {
        crate::structural_loop_eager::classify_loaded_methods(&mut file.methods);
        Arc::new(Self {
            native_state: crate::native_state::NativeStateFact::for_classfile_name(
                &file.this_class,
            ),
            file,
            statics: std::sync::RwLock::new(crate::FastMap::default()),
            initialized: AtomicBool::new(false),
            clinit_done: AtomicBool::new(false),
            init_owner: std::sync::atomic::AtomicU64::new(0),
            init_failed: AtomicBool::new(false),
            site_slots: std::sync::OnceLock::new(),
        })
    }
}

/// A member reference resolved once per call site, the way HotSpot's ResolvedMethodEntry is:
/// the three names AND the two counts derived from the descriptor, so a warm site never walks the
/// descriptor string again. `argc` is how many argument VALUES the call consumes (what the
/// interpreter pops); `slots` is how much room they need in the callee's locals (a bare J/D takes
/// two, JVMS 2.6.1).
///
/// It is one allocation shared by reference: handing callers an `Arc<SiteRef>` costs ONE atomic
/// increment, where cloning the old `(Arc<str>, Arc<str>, Arc<str>)` triple cost three on the way
/// in and three more on the way out — on every interpreted call and every field access.
pub struct SiteRef {
    pub owner: Arc<str>,
    pub name: Arc<str>,
    pub descriptor: Arc<str>,
    pub argc: u32,
    pub slots: u32,
    /// The callee this site resolves to: (declaring-class pointer, index into its `methods`).
    ///
    /// `Vm::invoke` looked the method up by STRING on every call — a name hash plus a descriptor
    /// memcmp across the overload set, 3.5% of an interpreted call. The answer is fixed for a
    /// monomorphic site, and this is the one place that is already borrowed per call, so caching
    /// it here costs no extra thread-local access and no extra hash. (A thread-local memo keyed on
    /// the three pointers was tried first and measured SLOWER than the lookup it replaced.)
    ///
    /// The class pointer is stored alongside so a site that somehow resolves elsewhere falls back
    /// to the lookup instead of indexing the wrong class.
    pub resolved: std::sync::OnceLock<(usize, u32)>,
    /// ⭐**THE WHOLE STEADY-STATE CALL DECISION, DECIDED ONCE** (default ON, `RAVA_NO_CALL_PLAN`).
    ///
    /// Row 395 prices an interpreted call whose callee is FREE — a static method with an
    /// empty body, served frame-free, no `Frame`, no allocation — at **105 ns against the
    /// JVM interpreter's 11 ns**. Nothing in that 105 ns is the callee. It is ~25 discrete
    /// steps the call site re-performs every time: two intercept gates keyed on `(owner,
    /// name)` strings that are FIXED for a site, a thread-local hash lookup, an
    /// `Arc<LoadedClass>` clone and drop, a `OnceLock` re-derivation of the method index,
    /// and then a second prologue of seven more gated census checks inside `invoke_idx`
    /// before the body is even looked at.
    ///
    /// Every input to those steps is constant for the lifetime of a monomorphic site, so
    /// the decision is made once and replayed. What is NOT constant stays on the fast path
    /// and is re-checked per call: the class-initialisation latch, and the trampoline flag.
    ///
    /// ⚠**A plan is installed only where skipping the prologue is unobservable** — never for
    /// a site the gson intercept could claim, never for an unresolvable/native-served site
    /// (those cache `None` and keep the slow path), and never from a GIL-free worker.
    pub plan: std::sync::OnceLock<CallPlan>,
}

/// A resolved call site's replayable entry (see `SiteRef::plan`).
pub struct CallPlan {
    /// The declaring class the site resolved to.
    pub decl: Arc<LoadedClass>,
    /// Its index into `decl.file.methods` — the name hash and descriptor memcmp, already done.
    pub midx: u32,
}

#[derive(Debug)]
pub enum LoadError {
    /// ⛔THE NAME IS AN `Arc<str>`, NOT A `String`, AND THE COUNT IS WHY. A boot performs
    /// **11,000,000 class lookups of which 4,512,560 (41.0%) are names ALREADY KNOWN
    /// ABSENT** — against 4,499 classes actually parsed. Every one of those used to build
    /// a fresh `String` for an error the caller discards, in the boot's first block, which
    /// is where the session's whole wall regression sits. Cloning the stored name is a
    /// refcount bump.
    NotFound(std::sync::Arc<str>),
    /// A GIL-free worker asked for a class that was not already published, and workers are
    /// forbidden to load (loading mutates the shared cache and moves the hierarchy epoch under
    /// every closure-based proof). The region has been aborted; this means "ask again on the
    /// serial path", NOT "this class does not exist".
    ///
    /// It is a separate variant because collapsing it into `NotFound` is a live defect: the
    /// interpreter's getstatic reads a `NotFound` as "a JDK class with no bytecode in the jar",
    /// falls through to the native static table, misses, and raises a FATAL
    /// `UnsupportedNative("getstatic <owner>.<field>")` — for a class that is on the classpath.
    /// That is the error an interpreter-only boot died on, and the same path is reachable under
    /// the shipped JIT-on configuration whenever a worker interprets a getstatic.
    WorkerRefused(String),
    Parse(String),
    Io(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotFound(n) => write!(f, "class not found: {n}"),
            LoadError::WorkerRefused(n) => write!(f, "worker refused to load: {n} (region aborted)"),
            LoadError::Parse(e) => write!(f, "class parse error: {e}"),
            LoadError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

/// A place classes/resources are searched: a directory or a jar.
///
/// ⭐**THE JAR IS BEHIND AN `RwLock`, NOT THE WHOLE CLASSPATH BEHIND A `Mutex`.** Only
/// `Jar::read_entry` needs `&mut` (the zip reader seeks and inflates); the name index,
/// the size table and the directory set are all `&self`. The old shape put every source
/// behind ONE `Mutex` and held it across the inflate, so a classpath scan — the bundled
/// server jar plus every library `add_classpath` adds — took an exclusive lock N times to
/// answer a question N−1 of the jars say "no" to from a hash set, and `has_resource` /
/// `resource_size` / `resource_is_dir` (the datapack reload's three questions per file)
/// queued behind whatever inflate was in flight.
///
/// Now a miss is a shared read and only the jar that actually HAS the entry is ever taken
/// exclusively. Costs one extra lock acquisition on the hit; the hit is the call that goes
/// on to inflate a whole classfile.
enum Source {
    Dir(PathBuf),
    Jar(std::sync::RwLock<Jar>),
}

/// The loader is shared by reference across every thread in the engine, so it must stay `Sync`
/// — and an `RwLock<Jar>` demands `Jar: Send + Sync` where the old `Mutex<Vec<Source>>` demanded
/// only `Send`. `Jar` holds plain tables plus a `ZipArchive<File>` (a `File` and an
/// `Arc<Shared>`), so it qualifies. This assertion exists so that a zip-crate upgrade which
/// introduces a `Cell` fails HERE, with a name and a reason, instead of at the seventeen
/// unrelated call sites that share a `&ClassLoader`.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Jar>();
    assert_send_sync::<Source>();
};

/// ⭐**THE ONE-SHOT CLASS-LOAD PATH** — default ON, `RAVA_NO_FAST_CLASSLOAD=1` disables, and so
/// does `RAVA_NO_EMIT_MORE` so the whole block bisects in one run.
///
/// Gates the three changes that take work off a PATH rather than off a call:
///   * `super_chain` splices the ancestor's already-memoised chain instead of re-walking to
///     `java/lang/Object` — a class's chain costs ONE loader lookup, not one per ancestor;
///   * `type_closure` unions its DIRECT parents' memoised closures instead of a breadth-first
///     walk of the whole hierarchy, which also memoises every ancestor on the way up, so a
///     hierarchy is walked once in total rather than once per class standing in it;
///   * the classpath scan probes a jar's name index under a shared lock before it takes that
///     jar's exclusive one, and asks the filesystem once instead of twice per directory source.
///
/// Off, every one of them falls back to the exact walk it replaced, so the saving is priced.
/// ⭐**AND IT IS READ OFF THE LOADER GATE WORD, NOT ITS OWN `OnceLock`.** Four call sites reach
/// this — `super_chain`'s splice, `type_closure`'s parent union, and the classpath scan in each of
/// `read_bytes` and `read_resource` — and every one of them reads two or three OTHER loader gates
/// in the same call. An `OnceLock` read is an acquire load the optimiser may not hoist or CSE, so
/// N of them stayed N; `loader::loader_gates()` resolves the whole family once per thread into a
/// const-initialised, destructor-free TLS word and each site reads a bit of it.
///
/// `RAVA_NO_FAST_LOADERSHAPE=1` (and `RAVA_NO_EMIT_MORE`) sends every read back through
/// [`fast_classload_live`] at its own site, which is exactly the code below.
pub(crate) fn fast_classload() -> bool {
    loader::lg_of(loader::loader_gates(), loader::LG_CLASSLOAD, fast_classload_live)
}

/// The historic reader. Called once per thread by the gate word's cold path, and per site only
/// under the kill switch.
pub(crate) fn fast_classload_live() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_CLASSLOAD").is_none() && !crate::flags::emit_more_disabled()
    })
}
/// Class names in a NATIVE-SERVED namespace for which a real classfile has been loaded.
///
/// Registering one flips `native_served_namespace` off for that name, which is the whole
/// mechanism behind migrating `java.util` from Rust natives to guest bytecode: the class stops
/// being intercepted and its own methods run. See `docs/java-util-as-bytecode.md`.
///
/// The set is only ever consulted through `has_bytecode`, and that reads one relaxed `bool`
/// first — with no overrides present (the default) the check costs a load and a branch on a
/// path that runs on every native dispatch.
static OVERRIDE_ANY: AtomicBool = AtomicBool::new(false);
static OVERRIDES: std::sync::OnceLock<std::sync::RwLock<HashSet<Box<str>>>> =
    std::sync::OnceLock::new();

fn overrides() -> &'static std::sync::RwLock<HashSet<Box<str>>> {
    OVERRIDES.get_or_init(|| std::sync::RwLock::new(HashSet::new()))
}

/// Record that `name` was loaded from a real classfile. Ignored unless the name lives in a
/// namespace the natives would otherwise claim — everything else is already guest code.
pub(crate) fn note_bytecode(name: &str) {
    // ⛔**`java/` ONLY — NOT the whole native-served list, and the difference broke the boot.**
    // `com/google/common/` and `it/unimi/` are in that list too, and unlike `java/` they DO ship
    // real bytecode on MC's classpath — which this engine deliberately does not run ("guava
    // collection classes whose own bytecode mis-executes in this interpreter ... which we serve
    // from the native map/collection layer instead", `opcodes/mod.rs`). Registering them as
    // overrides made every one of them stop being native-served, so the boot went off to
    // interpret guava's own collections: **40 s -> a 400 s timeout with the CPU pegged**, and
    // 4 gates plus 20 probes stayed green throughout.
    //
    // Overriding is for the `java.*` classes this project implements as natives and is migrating
    // back to bytecode. Nothing else is a candidate.
    // ⭐**ONE FAMILY AT A TIME** (`RAVA_GUEST_CLASSES=a/b/C,d/e/F`). The `java/` restriction above
    // exists because registering ALL of guava and fastutil took the boot from 40 s to a
    // 400-second timeout — but the receiver census says the collection mass sits in a HANDFUL of
    // named classes (`DoubleArrayList#getDouble` alone is 10.5 M calls a boot), and the inliner
    // that makes their bytecode worth running now exists and is proven on real traffic.
    //
    // So the door opens by NAME, for exactly the classes an experiment lists, and stays shut for
    // everything else. Empty by default: an unset variable is byte-identical to before.
    if !name.starts_with("java/") && !guest_class_allowed(name) {
        return;
    }
    let mut g = overrides().write().unwrap_or_else(|e| e.into_inner());
    if g.insert(name.into()) {
        OVERRIDE_ANY.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Has ANY native-served name been overridden by a real classfile in this process?
///
/// The cheap outer guard for every hot path that would otherwise pay for the possibility. One
/// relaxed load, and false for every run that does not use the override.
pub(crate) fn any_override() -> bool {
    OVERRIDE_ANY.load(std::sync::atomic::Ordering::Acquire)
}

/// Does this class have real bytecode that must win over the native of the same name?
pub(crate) fn has_bytecode(name: &str) -> bool {
    if !OVERRIDE_ANY.load(std::sync::atomic::Ordering::Acquire) {
        return false;
    }
    overrides()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains(name)
}

mod loader;
pub use loader::*;
/// The opt-in allowlist for `note_bytecode` — classes outside `java/` whose OWN bytecode should
/// win over the native of the same name. One comma-separated list, read once.
///
/// ⛔**MEASURED: OPENING THE DOOR IS NOT ENOUGH.** Listing
/// `it/unimi/dsi/fastutil/doubles/DoubleArrayList` — 10.5 M `getDouble` calls a boot, the largest
/// (class, method) pair in the engine — changes NOTHING: the receiver census still reports
/// 10,504,192 of them reaching the native. `note_bytecode` only fires when a classfile is
/// actually LOADED for that name, and a natively-served class is never loaded from the classpath
/// at all, so the registry has nothing to register.
///
/// ⇒ Migrating a family therefore needs the LOADER to be told to fetch it, not just the override
/// registry to accept it. That is the remaining step between here and the 8.12% the collection
/// population is worth, and it is a real change rather than a flag.
fn guest_class_allowed(name: &str) -> bool {
    static L: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let l = L.get_or_init(|| {
        std::env::var("RAVA_GUEST_CLASSES")
            .ok()
            .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default()
    });
    !l.is_empty() && l.iter().any(|c| c == name)
}
