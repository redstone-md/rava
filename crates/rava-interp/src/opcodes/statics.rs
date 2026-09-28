//! getstatic/putstatic and the multi-dimensional array allocation beside them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// `newarray` atype operand → element-type tag (JVMS Table 6.5.newarray-A).
pub(crate) fn primitive_array_type(atype: u8) -> &'static str {
    match atype {
        4 => "boolean",
        5 => "char",
        6 => "float",
        7 => "double",
        8 => "byte",
        9 => "short",
        10 => "int",
        11 => "long",
        _ => "int",
    }
}

/// Build the `L<name>;` reference-array element descriptor in a stack buffer and
/// run `f` on it (RAVA_ANEWARRAY_SCRATCH). The anewarray paths built this string
/// on the heap once per ALLOCATION — two mallocs of pure per-call bookkeeping on
/// a path the RIP profiler samples at 2.3-2.9% of the real boot. Falls back to
/// the heap for names longer than the buffer (none observed in practice).
pub(crate) fn with_ref_desc<R>(elem: &str, f: impl FnOnce(&str) -> R) -> R {
    // AN ARRAY COMPONENT IS ALREADY A DESCRIPTOR. `anewarray`'s operand is a
    // CONSTANT_Class whose name may itself be an array type — JVMS 4.4.1, and
    // javac emits exactly that for `new int[3][]` (`anewarray [I`) and
    // `new String[3][]` (`anewarray [Ljava/lang/String;`). Wrapping those in
    // `L...;` produced the malformed `L[I;`, which is not a descriptor at all.
    //
    // It stayed invisible while nothing read the element type back for a TYPE
    // DECISION. The three-valued `checkcast` does: `parse_elem("L[I;")` answers
    // `Elem::Class("[I")`, the target `[[I` answers `Elem::Array("I")`, and "a
    // plain object element is never an array" then threw ClassCastException on
    // `(int[][]) new int[3][]` — correct Java. `alloc_multi_array` right below
    // already stores the BARE `[I` for the identical runtime type, so this is
    // the convention the tree had everywhere else.
    if elem.starts_with('[') {
        return f(elem);
    }
    const CAP: usize = 192;
    let n = elem.len();
    if n + 2 <= CAP {
        let mut buf = [0u8; CAP];
        buf[0] = b'L';
        buf[1..=n].copy_from_slice(elem.as_bytes());
        buf[n + 1] = b';';
        // `'L' + <valid UTF-8> + ';'` is valid UTF-8; the checked constructor is
        // an O(n) scan with no allocation, kept over `unchecked` for safety.
        match std::str::from_utf8(&buf[..n + 2]) {
            Ok(d) => f(d),
            Err(_) => f(&format!("L{elem};")), // unreachable: input was a &str
        }
    } else {
        f(&format!("L{elem};"))
    }
}

/// Allocate a (possibly multi-dimensional) array for `multianewarray`. `desc`
/// is the array type descriptor (e.g. `[[I`, `[[Lcom/foo/Bar;`); `counts` holds
/// one size per allocated dimension. Dimensions beyond `counts.len()` stay null.
pub(crate) fn alloc_multi_array(vm: &crate::vm::thread_context::SharedVm, desc: &str, counts: &[usize]) -> u32 {
    let inner = desc.strip_prefix('[').unwrap_or(desc); // element type of this array
    let len = counts.first().copied().unwrap_or(0);
    if counts.len() <= 1 {
        // Leaf dimension: default-initialised elements (Null for refs/arrays,
        // 0 for primitives). Map the JVM type letter to alloc_array's convention.
        let elem: &str = match inner.as_bytes().first() {
            Some(b'[') | Some(b'L') => inner, // ref / sub-array → Null default
            Some(b'J') => "long",
            Some(b'D') => "double",
            Some(b'F') => "float",
            _ => "int", // I/Z/B/C/S → Int default
        };
        return vm.heap.alloc_array(elem, len);
    }
    let data: Vec<Value> = (0..len)
        .map(|_| Value::Array(alloc_multi_array(vm, inner, &counts[1..])))
        .collect();
    vm.heap.alloc_values(inner, data)
}

/// `getstatic` — `System.out` becomes the `StdOut` sentinel; otherwise read the
/// owner class's static field (running its `<clinit>` first).
/// ⚠**`#[inline(never)]` DELIBERATELY** — row 397: inlined into `opcodes::run`, this body's
/// locals are reserved by `run`'s prologue, which every interpreted call pays. `run` measured
/// a 2136-byte frame; the frame itself costs 160 ns against HotSpot's 7.2.
#[inline(never)]
#[allow(invalid_reference_casting)]
pub(crate) fn getstatic(vm: &crate::vm::thread_context::SharedVm, frame: &Frame, index: u16) -> Result<Value, VmError> {
    // Generation-guarded per-call-site cache, probed FIRST: the site key needs only
    // the class pointer + cp index, so a warm site skips `ref_parts` (three Arc<str>
    // refcount round-trips + the per-class RefCell) and the System.out/err string
    // compares entirely. The sentinels are cached like any other value — a gen bump
    // just re-resolves to the same sentinel.
    let site = (std::sync::Arc::as_ptr(&frame.class) as usize, index);
    if let Some(v) = crate::tlcache::getstatic_get(site, vm.runtime_shared().static_gen()) {
        return Ok(v);
    }
    // ⭐**BORROWED, NOT CLONED.** `ref_parts` handed back three `Arc<str>` — six atomic refcount
    // round-trips — for names this body only ever compares and hashes. `ref_site_ref` is the same
    // two loads and a null test that built them, minus the refcounting, and its borrow lives on
    // `frame`, which is shared here and so does not fight `&mut Vm`. Identical strings by
    // construction: `ref_parts` clones out of exactly this `SiteRef`.
    let s = ref_site_ref(&frame.class, index)?;
    let (owner, name, desc): (&str, &str, &str) = (&s.owner, &s.name, &s.descriptor);
    // ⭐**THE BOUND SITE: the static's home class, decided once.** Every getstatic that misses the
    // generation-keyed value cache above used to re-derive the class this access is ABOUT — a
    // `get_or_load` that hashes the owner NAME and clones an `Arc` out of the shared class cache,
    // then an out-of-line `ensure_init` to re-ask a question whose answer latched permanently the
    // first time it was asked. Both are constant once the owner's `<clinit>` has completed, so a
    // bound site goes straight to the storage. `Vm::static_site_decide` states what it must prove.
    //
    // This matters because the value cache above is invalidated WHOLESALE by `bump_static_gen`:
    // every putstatic anywhere in the process, and a boot's first half is one long sequence of
    // them, so the "miss" path here is the path a booting engine is actually on.
    let bound = Vm::static_site(site.0, index, name, |decl| {
        let cur = decl
            .statics
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned();
        field_or_default(cur.unwrap_or(Value::Null), desc)
    });
    let value = match bound {
        Some(v) => v,
        None if owner == "java/lang/System" && (name == "out" || name == "err") => {
            if name == "out" { Value::StdOut } else { Value::StdErr }
        }
        None => match getstatic_bind(vm, site.0, index, owner, name, desc) {
            Some(v) => v,
            None => getstatic_resolve(vm, owner, name, desc)?,
        },
    };
    crate::tlcache::getstatic_put(site, vm.runtime_shared().static_gen(), value.clone());
    Ok(value)
}

/// First execution of a getstatic site (or the first since it lost its slot): try to bind it, and
/// serve the read from the binding when the attempt succeeds.
///
/// The one loader probe this costs is the same probe `getstatic_resolve` would have made a moment
/// later, and it is not repeated: a site that cannot be bound is marked `Refused` (or `Pending`,
/// when the owner is merely still initialising) and `static_site_open` turns the attempt off.
///
/// Returns `None` — with nothing stored, allocated or mutated on the way out beyond the slot's
/// decision word — for every shape it cannot prove, and the caller runs the untouched resolver.
#[cold]
fn getstatic_bind(
    vm: &Vm,
    caller: usize,
    cp: u16,
    owner: &str,
    name: &str,
    desc: &str,
) -> Option<Value> {
    // A GIL-free worker does not attempt: `get_or_load` may answer `WorkerRefused`, whose ONLY
    // correct handling is the region abort `getstatic_resolve` performs, and a worker has no
    // business deciding a site the serial path will decide better a moment later.
    if !Vm::static_site_open(caller, cp) || crate::parallel::gilfree() {
        return None;
    }
    let Ok(class) = vm.loader.get_or_load(owner) else {
        // Unloadable here means the natives serve this static (`java/lang/*` with no bytecode in
        // the jar). Refusing is free of consequence: the slow path below still runs in full, so if
        // the class ever does become loadable the answer simply comes from there.
        Vm::static_site_refuse(caller, cp);
        return None;
    };
    if !Vm::static_site_decide(caller, cp, &class, name) {
        return None;
    }
    let cur = class
        .statics
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(name)
        .cloned();
    Some(field_or_default(cur.unwrap_or(Value::Null), desc))
}

/// ⚠**`#[inline(never)]` DELIBERATELY** — row 397: inlined into `opcodes::run`, this body's
/// locals are reserved by `run`'s prologue, which every interpreted call pays. `run` measured
/// a 2136-byte frame; the frame itself costs 160 ns against HotSpot's 7.2.
#[inline(never)]
pub(crate) fn putstatic(vm: &crate::vm::thread_context::SharedVm, frame: &mut Frame, index: u16) -> Result<(), VmError> {
    // THE SAME GATE THE COMPILED PUTSTATIC HAS. A static write publishes shared state, and a
    // GIL-free worker may not do that unobserved — jit/callbacks/fields.rs takes exactly this gate
    // (kind 2), and jit/tests/gilfree_worker.rs pins the contract. The interpreter had no gate at
    // all, and workers DO interpret guest bytecode: every fork shape runs the real body on its own
    // frame. Under RAVA_M3_WORKER_LOCKED_EXEC the write proceeds under the resolve lock, exactly
    // as the compiled path does; otherwise the region aborts without publishing.
    #[cfg(feature = "next-gen-gc")]
    let _worker_gate = match crate::jit::worker_illegal_gate(2, "putstatic") {
        crate::jit::WorkerGate::Serial => None,
        crate::jit::WorkerGate::Locked(g) => Some(g),
        crate::jit::WorkerGate::Abort => return Err(VmError::RegionAbort),
    };
    let caller = std::sync::Arc::as_ptr(&frame.class) as usize;
    // Borrowed, not cloned — see the note on `getstatic`. Taken as a raw pointer only so that the
    // `frame.pop()` below (which needs `&mut Frame`) can keep its ORIGINAL position: swapping the
    // two would change which error a malformed classfile reports.
    // SAFETY: the `SiteRef` is a leaked `Box` owned by `frame.class`'s slot table and freed only
    // in `Drop for LoadedClass`; the frame holds an `Arc` to that class for this whole call, and
    // popping an operand does not touch the table.
    let s: *const crate::class_loader::SiteRef = ref_site_ref(&frame.class, index)?;
    let value = frame.pop()?;
    let s = unsafe { &*s };
    let (owner, name): (&str, &str) = (&s.owner, &s.name);
    // Invalidate the getstatic cache: any static write bumps the generation
    // (also refreshes the native getstatic mirror compiled code reads).
    vm.bump_static_gen();
    // BOUND SITE (see `getstatic`): the class that holds this field's storage, and the fact that
    // its `<clinit>` has completed, were decided the first time this site ran. A repeated write —
    // a counter, a lazily rebuilt cache, a registry that is re-seeded per reload — skips the
    // owner-name hash of `get_or_load`, the `Arc` clone out of the shared class cache, the
    // `ensure_init` call, and the LINEAR scan of `class.file.fields` below.
    let mut pending = Some(value);
    let stored = Vm::static_site(caller, index, name, |decl| {
        store_static(decl, name, pending.take().unwrap_or(Value::Null));
    });
    if stored.is_some() {
        return Ok(());
    }
    let value = pending.take().unwrap_or(Value::Null);
    let class = vm.loader.get_or_load(owner)?;
    // One relaxed load for a class that finished initialising long ago, instead of an out-of-line
    // call, a SeqCst global probe and three more loads. Every other §5.5 state falls through to
    // the real `ensure_init`.
    vm.ensure_init_shared(&class)?;
    // The owner names its OWN field if it declares one (JLS 8.3: a subclass and
    // its superclass may both declare a same-named static — those are DISTINCT
    // fields and must not alias). Only when the owner merely INHERITS the static
    // (does not declare it) do we walk up to the declaring superclass, so an
    // inherited static stays in one place.
    let owner_declares = class.file.fields.iter()
        .any(|f| f.name.as_str() == name && f.access_flags & 0x0008 != 0);
    // The owner-declares shape is the one the binding can prove, so record it HERE, where the
    // walk has just been paid for anyway — the next write at this site replays it. Gated on
    // `static_site_open` so a site already decided is not re-decided (and its `Arc` not re-cloned)
    // on every store; an inherited static is simply never bound and keeps the walk below forever.
    if owner_declares && Vm::static_site_open(caller, index) {
        Vm::static_site_decide(caller, index, &class, name);
    }
    if !owner_declares {
        for cn in vm.super_chain(owner).iter() {
            if cn.as_ref() == owner { continue; }
            if let Ok(c) = vm.loader.get_or_load(cn) {
                if c.file.fields.iter().any(|f| f.name.as_str() == name && f.access_flags & 0x0008 != 0) {
                    store_static(&c, name, value);
                    return Ok(());
                }
            }
        }
    }
    store_static(&class, name, value);
    Ok(())
}

/// Write a static.
///
/// ⛔**THE "AVOID THE ALLOCATION" VERSION WAS MEASURED AND REVERTED.** `insert(name.to_string(),
/// v)` allocates a `String` on every write, and `HashMap::insert<String, Value>` shows at 6-8%
/// of a `putstatic`-dominated microbenchmark — the shape this tree has fixed three times
/// elsewhere. Probing borrowed with `get_mut` first and only allocating for a new key looked
/// like the same fix: it took `static put int` from 260 to 185 ns.
///
/// The BOOT said otherwise. Same-binary A/B (`RAVA_NO_STATIC_STORE_MEMO`, n=4, `-Jit`): the
/// ALLOCATING form is **2.81% faster**, winning three of four pairs. The reason is the
/// workload: a static is written ONCE, in `<clinit>`, so the `get_mut` probe MISSES and the
/// write pays two lookups instead of one. A microbenchmark that writes the same static a
/// million times measures the opposite of what a boot does.
/// How many static writes does a boot actually execute? (`RAVA_PUTSTATIC_COUNT`.)
///
/// The write itself is expensive — `bench/StoreCost.java` prices a compiled `putstatic int` at
/// **226 ns against HotSpot's 0**, while `putfield`/`aastore`/`aaload` all measure 1-3 ns and
/// match HotSpot exactly. The cost is visible right below: a write lock, a `String` ALLOCATION
/// for the key, and a hash insert, on every single store.
///
/// But a per-op price is not a share of the boot, so count the population before touching it.
pub(crate) static PUTSTATIC_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn store_static(class: &crate::class_loader::LoadedClass, name: &str, value: Value) {
    // Armed by the diagnostics registry too (`RAVA_DIAG=statics`), because the question this
    // counter exists to answer — "a per-op price is not a share of the boot, so count the
    // population" — was never actually answered on a product boot.
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ARMED.get_or_init(|| {
        crate::env_flag!("RAVA_PUTSTATIC_COUNT")
            || crate::diag::selected("statics", "writes")
    }) {
        PUTSTATIC_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    class
        .statics
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name.to_string(), value);
}

/// An unset field reads as its type's default (0/0L/0.0/false → Int/Long/...,
/// references → null) rather than a bare Null, matching JVM field semantics.
pub(crate) fn field_or_default(v: Value, descriptor: &str) -> Value {
    if !matches!(v, Value::Null) {
        return v;
    }
    match descriptor.bytes().next() {
        Some(b'J') => Value::Long(0),
        Some(b'F') => Value::Float(0.0),
        Some(b'D') => Value::Double(0.0),
        Some(b'Z') | Some(b'B') | Some(b'C') | Some(b'S') | Some(b'I') => Value::Int(0),
        _ => Value::Null,
    }
}

#[cfg(feature = "next-gen-gc")]
/// ⭐**`&SharedVm`, WITH THE ONE CAST INSIDE.** The mutation is `gc_protect.push/extend`
/// on a plain `Vec` field — the same shape every other caller-side rooter had when this
/// migration began — and the body is reached once per interpreted call, so the cast lives
/// HERE, at the one definition, instead of a cast per call site in three opcode files
/// (`invokestatic`, `invokespecial`, `invokevirtual`) and five JIT seams. Centralize the
/// transition behind the same `Heap` `as_mut_transitional` chokepoint: one spelling, one
/// `allow`, every caller compiles as-is through the `&Vm → &SharedVm` coercion
/// (`SharedVm == Vm`, a type alias — `&mut Vm` still coerces to `&SharedVm`).
///
/// The guard handed back borrows `gc_protect` as `*mut` with NO lifetime, exactly as
/// before — the truncation-on-drop contract is unchanged, and the raw pointer is derived
/// from the cast `&mut` for the duration of the push only.
#[allow(invalid_reference_casting)]
pub(crate) fn push_call_roots(
    vm: &crate::vm::thread_context::SharedVm,
    receiver: Option<&Value>,
    args: &[Value],
) -> CallRootGuard {
    // A GIL-free fork worker must NOT touch the shared gc_protect (concurrent push/truncate races).
    // No GC runs inside a fork region (workers abort on any nursery pressure, main is parked), so
    // rooting the invoke args is unnecessary here — return a no-op guard.
    if crate::parallel::gilfree() {
        return CallRootGuard(std::ptr::null_mut(), 0);
    }
    let base = crate::vm::thread_context::root_len(&vm.gc_protect);
    // Only heap/arena references can be swept or moved — primitives never need rooting, so
    // skip them to keep gc_protect small on the hot invoke path.
    let is_ref = |v: &Value| matches!(v, Value::Ref(_) | Value::Array(_) | Value::Obj(_));
    // THE transitional cast, the exact spelling `getstatic_resolve`/`putstatic` use — minted
    // HERE (the one definition) so every call site in every opcode file compiles cast-free.
    let vm_mut = unsafe { &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm) };
    let gcp = &mut vm_mut.gc_protect;
    if let Some(r) = receiver {
        if is_ref(r) {
            crate::vm::thread_context::push_root(gcp, r.clone());
        }
    }
    crate::vm::thread_context::extend_roots(
        gcp,
        args.iter().filter(|v| is_ref(v)).cloned(),
    );
    CallRootGuard(gcp as *mut _, base)
}

/// RAVA_ARGBUF_RECYCLE (default OFF) — return the pooled argument buffer to the
/// thread-local pool on the NATIVE dispatch paths, which drop it today.
///
/// `pop_args` checks a buffer out of `frames::pool_take`, and `Vm::invoke`
/// returns it — but only for calls that reach guest BYTECODE. A call served by
/// the JDK native layer only ever BORROWS the buffer (`try_invoke(.., &locals)`)
/// and then lets it fall out of scope, so the buffer never goes back. The
/// `RAVA_VECGROW_STAT` census over blocks A+B measures the consequence:
///
///   POOL take=50,138,719  give=28,679,915  **LEAKED=21,458,804**  miss=43.00%
///   EVENT invokevirtual -> NATIVE branch  17,337,632
///   EVENT invokestatic  -> NATIVE branch   1,420,732
///   EVENT invokespecial -> NATIVE branch     306,839
///
/// A pool that is drained 43% of the time is not a pool: `pool_take` hands back
/// `Vec::new()`, and the caller immediately grows it — which is precisely the
/// `RawVecInner::finish_grow` the RIP profile names as the dominant caller of
/// `mi_malloc_aligned` (5.5% self, allocator cluster 11.7% of A+B). The census
/// confirms the chain end to end: **99.96%** of the 12.7M `insert(0, receiver)`
/// grows start from capacity ZERO, i.e. from a pool miss rather than from a
/// buffer that was merely one slot short.
///
/// This CHEAPENS AN OPERATION (an allocation stops happening); it duplicates no
/// body and adds no per-call test to the dispatch path — the recycle happens on
/// the way out, where the buffer was being discarded anyway.
/// DEFAULT-ON since 2026-07-29 (`RAVA_NO_ARGBUF_RECYCLE` opts out).
///
/// COUNTED (`RAVA_VECGROW_STAT`, contention-immune): armed, the pool goes from
/// `miss 29.25%, LEAKED 18,425,971` to `miss 5.41%, LEAKED 1,327,345` - about 17M allocate+free
/// pairs per boot replaced by reuse.
///
/// TIMED: 10 clean runs, TWO batches with REVERSED order, pcore, interleaved (runs whose wall
/// exceeded 80s were contended and are excluded; they hit both arms):
///
/// ```text
/// on   Done 22.256 22.412 22.428 22.884 23.058   min 22.256  mean 22.608   wall mean 76.18
/// off  Done 22.600 22.825 22.948 23.879 24.375   min 22.600  mean 23.325   wall mean 76.30
/// ```
///
/// Done −1.5% on the minima and −3.1% on the means, consistent across both batches; wall neutral,
/// which is what a ~0.7s Done gain looks like inside a 76s wall. ⚠ NOT strictly "ON CLEAN" - one
/// overlap (23.058 vs 22.600) - and that is stated rather than smoothed over. It ships because the
/// timed evidence is PRESENT and POSITIVE on both statistics as well as counted, which is exactly
/// what the two changes reverted earlier this session lacked.
pub(crate) fn argbuf_recycle() -> bool {
    pub(crate) static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RAVA_NO_ARGBUF_RECYCLE").is_none())
}

/// A `Vec<Value>` checked out of the thread-local pool that returns ITSELF to
/// the pool when it goes out of scope — including on a `?` early return, which
/// is why this is a guard and not a trailing `give_vec` call.
///
/// GC contract, identical to `natives::CanonArgs`: the buffer is CLEARED before
/// it is pooled, so the pool never holds a `Value` across a GC point. It is only
/// ever dropped after the native call has returned, by which point
/// `NativeArgsGuard` has already unrooted the slice it published.
///
/// [`Self::into_inner`] hands ownership on to a callee that recycles the buffer
/// itself (`Vm::invoke`, or the trampoline's `pending` slot), so a buffer is
/// never returned to the pool twice.
pub(crate) struct PooledVals {
    v: Option<Vec<Value>>,
    /// Read ONCE at construction, not per drop — the flag decision is hoisted
    /// off the dispatch path, and it makes the guard unit-testable.
    recycle: bool,
}

impl PooledVals {
    #[inline]
    pub(crate) fn new(v: Vec<Value>) -> Self {
        Self { v: Some(v), recycle: argbuf_recycle() }
    }
    #[cfg(test)]
    pub(crate) fn with_recycle(v: Vec<Value>, recycle: bool) -> Self {
        Self { v: Some(v), recycle }
    }
    /// Give the buffer to an owner that will recycle it, disarming this guard so
    /// the same buffer can never be returned to the pool twice.
    #[inline]
    pub(crate) fn into_inner(mut self) -> Vec<Value> {
        self.v.take().unwrap_or_default()
    }
}

impl std::ops::Deref for PooledVals {
    type Target = Vec<Value>;
    #[inline]
    fn deref(&self) -> &Vec<Value> {
        self.v.as_ref().expect("PooledVals used after into_inner")
    }
}

impl std::ops::DerefMut for PooledVals {
    #[inline]
    fn deref_mut(&mut self) -> &mut Vec<Value> {
        self.v.as_mut().expect("PooledVals used after into_inner")
    }
}

impl Drop for PooledVals {
    #[inline]
    fn drop(&mut self) {
        if let Some(mut v) = self.v.take() {
            // Flag OFF: drop exactly as before, so the arm is byte-identical.
            if self.recycle {
                v.clear();
                crate::frames::pool_give(v);
            }
        }
    }
}

/// Pop `arg_count` operands into a POOLED buffer, in source order (first pushed
/// = first arg). The buffer is recycled by `invoke`/`give_vec` after the callee
/// drains it — the per-call argument Vec was the single largest allocator source.
pub(crate) fn pop_args(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    desc: &str,
) -> Result<Vec<Value>, VmError> {
    pop_args_n(vm, frame, descriptor::arg_count(desc))
}

/// `pop_args` for a caller that already knows the count — every interpreted call site does, once
/// its member ref is resolved, and re-deriving it means walking the descriptor string per call.
///
/// ⭐**`&SharedVm`**: the ONLY `&mut Vm` need was `vm.take_vec()` — a thread-local pool
/// checkout that never touches shared `Vm` state — and `take_vec` is now `&self`. So this
/// runs on the plain shared reference with NO transitional cast at all: the frame is the
/// `&mut` operand, exactly as before.
pub(crate) fn pop_args_n(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    n: usize,
) -> Result<Vec<Value>, VmError> {
    let len = frame.stack_len();
    if len < n {
        return Err(VmError::StackUnderflow);
    }
    let mut buf = vm.take_vec();
    // ⭐**A PROCESS CONSTANT, READ THROUGH AN `OnceLock` ON EVERY INTERPRETED CALL.** The
    // `vecgrow::note` at the bottom of this function opens with `enabled()`, which is a
    // `OnceLock` ACQUIRE load behind an out-of-line call over `RAVA_VECGROW_STAT` — an
    // environment variable whose answer is fixed before the first guest opcode runs. On x86
    // the load itself is a `mov`; the cost is that LLVM may not hoist it, may not fold it
    // with the census gates the frame constructor reads three lines later, and must treat
    // the memory it straddles as clobbered — which here means the `capacity()` below is
    // materialised, and kept live across the whole copy, for a counter nobody armed. Ledger
    // row 405 priced exactly this shape from the other side.
    //
    // `vmframe::frame_census` is the SAME gate, already resolved once per thread into the
    // byte `size_frame_buffer` and `Frame::new` read — no second piece of state, nothing to
    // keep in sync, and `RAVA_NO_FRAME_GATES=1` puts both sites back on the `OnceLock`. When
    // the census is off this function now contains no census code at all.
    let census = crate::vmframe::frame_census();
    let cb = if census { buf.capacity() } else { 0 };
    // RESERVE n+1, NOT n. Every virtual/special caller prepends the receiver right after this
    // returns (`locals.insert(0, receiver)`), so a buffer sized for the arguments alone is always
    // one slot short. `extend` from a drain is exact-size, so on a pool MISS the sequence today is
    // two allocations: one of n at the extend, then a doubling grow at the insert. The vec-growth
    // census shows the second one plainly - 13,381,475 calls at `invoke_resolved insert(0,recv)`,
    // 19.13% of them growing, 99.96% of those from capacity ZERO.
    //
    // This is a strict reduction with nothing added: on a pool HIT the buffer is already large
    // enough and `reserve` is a no-op, and on a MISS one right-sized allocation replaces a grow
    // chain. It does NOT address the drained pool itself (that is RAVA_ARGBUF_RECYCLE, still
    // parked OFF and owed a clean A/B) and it does not remove the insert's memmove.
    //
    // ⭐**AND ON A MISS `n + 1` IS THE WRONG NUMBER — IT BUYS THE SECOND ALLOCATION BACK**
    // (`RAVA_NO_FAST_POOLLEAK`). What this buffer becomes is a FRAME: `Frame::build` hands it to
    // `vmframe::size_frame_buffer`, which needs `max_locals + max_stack`. Reserving `n + 1` is
    // right for a pool HIT (already big enough, so the whole thing is a not-taken compare) and
    // wrong for a MISS — it allocates exactly the arguments, and the frame constructor then
    // reallocates it: `mi_malloc`, memcpy, free, for a buffer the process already owned and had
    // handed back to the allocator. `vmframe::fast_pool2_uncached` and `frames::POOL_CAP` both
    // state this in as many words and neither acts on it, and the pool's own census gives the
    // population: 3,469,985 misses, of which `LEAKED (1,330,047) + GIVE_FULL (2,132,888)` is
    // 99.8% — neither cause fixable from here (the leaks are `Vm::invoke_idx`'s early returns,
    // and the ceiling was measured at 4096 and cost 100 MB for a quarter of a point).
    //
    // So a miss asks for what a FRAME on this thread has just been measured to need.
    // `frames::pool_size_hint()` is the capacity of the last buffer this thread returned to the
    // pool, clamped by `frames::POOL_HINT_SLOTS`, and zero when unobserved or when the kill
    // switch is engaged — `max(need)` is what turns that zero into a no-op, so the OFF arm is
    // the historic `reserve(n + 1)` with no gate read of its own.
    //
    // ⚠**THE HINT IS READ ONLY WHEN THE CAPACITY IN HAND IS ALREADY SHORT** — the branch that
    // was going to call the allocator anyway (5.38% of takes). Ledger rows 445-448 keep
    // restating the rule this obeys: a read's worth is its POPULATION, and touching a per-thread
    // word on all 64.4M calls to serve 3.5M of them is the shape that has measured neutral five
    // times. `reserve` performs this same capacity compare internally, so on the hit path this
    // is the identical not-taken branch it always was, now spelled out so the hint can hide
    // behind it.
    //
    // NOT the refuted experiment: `jit_run`'s `cells` capacity was deliberately left at
    // `max_locals` after sizing it "correctly" was built and measured WORSE at lane width 6 - see
    // the long comment at that site. This is a different buffer, on a different path.
    let need = n + 1;
    // `reserve` is relative to LEN, so the capacity test must be too. `buf` comes from the pool
    // with length 0 in every case that exists today; the subtraction is written the general way
    // because a future taker handing in a non-empty buffer must not underflow here.
    if buf.capacity() - buf.len() < need {
        buf.reserve(need.max(crate::frames::pool_size_hint()));
    }
    // A BITWISE MOVE, not a drain-extend. Draining the tail of a vec into another vec is a move of
    // n values and nothing else — the source tail is empty, so `Drain`'s whole job (shifting the
    // remainder back, and a drop guard in case the consumer panics mid-iteration) is dead weight.
    // The generated `Vec::extend_trusted<Value, Drain>` was 7.6% of an interpreted call in the
    // microbench profile, on a path whose real content is a memcpy of n*24 bytes.
    let dst_len = buf.len();
    // SAFETY: `len >= n` is checked above, `reserve` guarantees the capacity, the two buffers are
    // distinct allocations, and ownership of the n values moves with the bytes — the source is
    // truncated without dropping them, exactly as a drain would.
    unsafe {
        std::ptr::copy_nonoverlapping(
            frame.stack().as_ptr().add(len - n),
            buf.as_mut_ptr().add(dst_len),
            n,
        );
        buf.set_len(dst_len + n);
        // `len` is an OPERAND count; the buffer holds the locals in front of them. `set_stack_len`
        // and NOT `truncate_stack`: the `n` values above were just MOVED into `buf`, so dropping
        // them here would free them twice.
        frame.set_stack_len(len - n);
    }
    if census {
        crate::vecgrow::note(crate::vecgrow::Site::PopArgs, cb, buf.capacity(), n);
    }
    Ok(buf)
}

/// `RAVA_NO_FAST_CALLPATH=1` — kill switch for the single-move instance-call prologue below.
/// Default ON; also disabled wholesale by `RAVA_NO_EMIT_MORE`.
///
/// Read once per instance call, on the branch that is about to move the operands anyway — the
/// same place `PooledVals::new` reads `argbuf_recycle`.
pub(crate) fn fast_callpath() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_CALLPATH").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Pop the receiver AND `n` arguments of an instance call into a pooled buffer, already laid out
/// as the callee's locals — `[receiver, arg1, … argN]`.
///
/// ⭐**ONE FACT, DERIVED THREE TIMES: "the callee's locals".**
///
/// JVMS 6.5 fixes the operand layout of `invokespecial`/`invokevirtual`/`invokeinterface` as
/// `objectref, arg1, … argN`, pushed in that order — so the receiver sits DIRECTLY BELOW the
/// arguments and the callee's locals are that contiguous run, in that exact order, already. The
/// interpreter nevertheless took the run apart and put it back together on every instance call:
///
///   1. `pop_args_n` moved the N arguments out into a pooled buffer,
///   2. `frame.pop()` lifted the receiver off separately, one slot further down,
///   3. `locals.insert(0, receiver)` **memmoved the whole argument vector one slot right** to
///      re-open the hole step 1 had just closed.
///
/// Steps 2 and 3 exist only to undo part of step 1. `Vec::insert` at index 0 is a bounds check,
/// a capacity branch and a `ptr::copy` of `N * 24` bytes; the vec-growth census quoted on
/// `pop_args_n` above counts **13,381,475** executions of that one line at the resolved virtual
/// site alone, and `invokespecial` adds every constructor call in the program — a `new` executes
/// TWO of them, the class's own `<init>` and the implicit `java/lang/Object.<init>` super.
///
/// This moves `N + 1` values ONCE, straight from the operand stack into the buffer that becomes
/// the callee's locals. The capacity request is unchanged: `pop_args_n` already reserves `n + 1`
/// "because every virtual/special caller prepends the receiver right after this returns", and
/// that reservation is now what actually holds the receiver instead of paying for the memmove.
///
/// ⚠**THE GUARD MAY ONLY REJECT.** The fast arm requires `stack_len >= n + 1`, which is strictly
/// stronger than the `stack_len >= n` the historic step 1 required; anything short falls through
/// to the byte-identical two-step below, so the underflow it reports — including which of the two
/// pops reports it and the `RAVA_TRACE_UNDERFLOW` line that goes with it — is unchanged.
///
/// ⚠**GC:** nothing between the two arms is a collection point (`take_vec` is a thread-local pool
/// hand-off, the copy is bitwise, `Vec::insert` may call the Rust allocator but never the guest
/// collector), so a caller that roots the whole returned slice with `push_call_roots(vm, None, ..)`
/// roots exactly the values — receiver first, then the arguments, in that order — that
/// `push_call_roots(vm, Some(&receiver), &args)` used to root, at a point no GC can fall between.
pub(crate) fn pop_recv_args_n(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    n: usize,
) -> Result<Vec<Value>, VmError> {
    let need = n + 1;
    let len = frame.stack_len();
    if len >= need && fast_callpath() {
        // In a debug build, record the SHAPE of the run we are about to move so the assert at the
        // bottom can prove the buffer came out as `[receiver, args..]` in stack order — the one
        // property the two arms must agree on. Discriminants only: `Value` has no `PartialEq`.
        #[cfg(debug_assertions)]
        let want: Vec<std::mem::Discriminant<Value>> = frame.stack()[len - need..]
            .iter()
            .map(std::mem::discriminant)
            .collect();
        let mut buf = vm.take_vec();
        // The same resolved-once census gate `pop_args_n` reads, for the same reason.
        let census = crate::vmframe::frame_census();
        let cb = if census { buf.capacity() } else { 0 };
        let dst_len = buf.len();
        // Byte-for-byte the reservation `pop_args_n` already made (`n + 1`, widened to the
        // thread's last observed frame size only when the buffer in hand is short) — see the long
        // note there. Nothing new is requested; the slot is simply used.
        if buf.capacity() - buf.len() < need {
            buf.reserve(need.max(crate::frames::pool_size_hint()));
        }
        // SAFETY: `len >= need` is checked above, `reserve` guarantees the capacity, the two
        // buffers are distinct allocations, and ownership of the `need` values moves with the
        // bytes — the source is truncated with `set_stack_len`, which does NOT run drop glue on
        // slots whose contents have been moved out (see `Frame::set_stack_len`).
        unsafe {
            std::ptr::copy_nonoverlapping(
                frame.stack().as_ptr().add(len - need),
                buf.as_mut_ptr().add(dst_len),
                need,
            );
            buf.set_len(dst_len + need);
            frame.set_stack_len(len - need);
        }
        if census {
            crate::vecgrow::note(crate::vecgrow::Site::PopArgs, cb, buf.capacity(), need);
        }
        #[cfg(debug_assertions)]
        {
            let got: Vec<std::mem::Discriminant<Value>> =
                buf[dst_len..].iter().map(std::mem::discriminant).collect();
            debug_assert_eq!(
                got, want,
                "pop_recv_args_n must yield [receiver, args..] in operand-stack order"
            );
        }
        return Ok(buf);
    }
    // COLD ARM — the historic three-step, verbatim, for a short stack or the kill switch.
    let args = pop_args_n(vm, frame, n)?;
    let receiver = frame.pop()?;
    let mut locals = args;
    locals.insert(0, receiver);
    Ok(locals)
}
