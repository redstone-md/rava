//! Runtime method metadata.

use crate::classfile::CodeInfo;
use crate::klass::{Klass, ACC_ABSTRACT, ACC_NATIVE, ACC_PRIVATE, ACC_STATIC, ACC_SYNCHRONIZED};
use crate::natives::NativeFn;
use crate::symbol::Symbol;
use crate::types::MethodSig;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;

pub struct Method {
    pub holder: &'static Klass,
    pub name: Symbol,
    pub desc: Symbol,
    pub access: u16,
    pub sig: MethodSig,
    pub code: Option<&'static CodeInfo>,
    /// Index in the holder's (and every subclass's) vtable; -1 when not virtually dispatched;
    /// for an interface's method, -2 - its slot in an itable block of the interface (what
    /// generated code dispatching a `Method*` known only at run time reads).
    pub(crate) vtable_index: AtomicI32,
    /// Whether this native runs without an interpreter frame of its own (`jit::leaf_native`).
    pub(crate) leaf_native: OnceLock<bool>,
    /// Invocation and loop back-edge counters feeding tier-up and on-stack replacement
    /// (generic JVM adaptivity). Racy increments are acceptable: they only time the tier-up.
    pub invocations: AtomicU32,
    pub backedges: AtomicU32,
    /// Machine-code entry with the compiled-code ABI (`jit::EntryFn`): compiled code, or an
    /// adapter into the interpreter / native. Generated code calls through this field.
    pub(crate) entry: AtomicUsize,
    /// Where a register-convention call to this method goes now (the target of
    /// `reg_trampoline`), for calls that find the method at run time (vtable dispatch); 0
    /// until the trampoline is made.
    pub(crate) reg_call: AtomicUsize,
    /// What only methods that run natively, get compiled or are named by method handles need
    /// (`MethodJit`), made on first need: most of a large program's methods never do, and
    /// these fields made up half of every method.
    jit: std::sync::atomic::AtomicPtr<MethodJit>,
    /// Set once compilation was attempted and refused, so it is not retried.
    pub(crate) not_compilable: std::sync::atomic::AtomicBool,
    hidden: OnceLock<bool>,
    /// Set once tier-2 compilation was requested.
    pub(crate) tiered: std::sync::atomic::AtomicBool,
    /// The first compilation has been requested (`jit/queue.rs`).
    pub(crate) queued: std::sync::atomic::AtomicBool,
    /// The VM supplies the body (`NativeRegistry::add_stub`).
    stub: bool,
    /// Some loaded class overrides this method (class hierarchy analysis): compiled code that
    /// inlined it without a receiver check tests this byte (in low memory, `crate::low`) and
    /// deoptimizes once it is set.
    overridden: &'static std::sync::atomic::AtomicBool,
}

/// The parts of a method only some methods need (`Method::jit`).
#[derive(Default)]
pub(crate) struct MethodJit {
    pub(crate) native: OnceLock<NativeFn>,
    /// The native code JNI bound this native method to (`jni::entry`), 0 until bound.
    pub(crate) jni_entry: AtomicUsize,
    /// A `jmp rel32` to `entry` for direct calls from compiled code (`code_cache::direct_entry`),
    /// repointed whenever `entry` changes; 0 until a call site wants one.
    pub(crate) trampoline: AtomicUsize,
    /// A `jmp rel32` for direct calls in the register convention (`jit::abi`): to the
    /// optimized code's register entry, else to `reg_adapter`; repointed with `trampoline`.
    /// 0 until a call site wants one.
    pub(crate) reg_trampoline: AtomicUsize,
    /// The adapter from the register convention to `entry` (`code_cache::reg_adapter`), made
    /// once on first need; 0 until then.
    pub(crate) reg_adapter: AtomicUsize,
    /// The `ResolvedMethodName` naming this method, made once and kept for good (HotSpot's
    /// `ResolvedMethodTable` interns them likewise); 0 until a `MemberName` resolves to it.
    pub(crate) resolved_name: AtomicUsize,
    /// Current compiled code (replaced on recompilation; old code stays valid for frames still
    /// running it, since every version shares the frame layout).
    compiled: std::sync::atomic::AtomicPtr<crate::jit::CompiledMethod>,
    /// The latest baseline-compiled code (with block entry points), which frames already
    /// running it keep using after optimized code is installed.
    baseline: std::sync::atomic::AtomicPtr<crate::jit::CompiledMethod>,
    /// Optimized versions entered from a running loop (`jit/opt/osr.rs`), newest first.
    osr: std::sync::atomic::AtomicPtr<crate::jit::OsrCode>,
    pub(crate) recompiles: AtomicU32,
    /// Recompilations of optimized code after deoptimizations (`jit/opt`).
    pub(crate) deopt_recompiles: AtomicU32,
    /// Recompilations for call sites that turned hot after compilation (`jit::runtime`).
    pub(crate) reprofiles: AtomicU32,
    /// This native has blocked the thread (`interp::note_blocking`).
    pub(crate) blocks: std::sync::atomic::AtomicBool,
}

impl Method {
    /// The method's `MethodJit`, made now if it has none yet.
    pub(crate) fn jit(&self) -> &'static MethodJit {
        if let Some(j) = self.jit_if() {
            return j;
        }
        let made = Box::into_raw(Box::<MethodJit>::default());
        match self.jit.compare_exchange(std::ptr::null_mut(), made, Ordering::AcqRel, Ordering::Acquire) {
            // SAFETY: published once and never freed.
            Ok(_) => unsafe { &*made },
            Err(other) => {
                // SAFETY: ours, never published; the other is published and never freed.
                unsafe {
                    drop(Box::from_raw(made));
                    &*other
                }
            }
        }
    }

    /// The method's `MethodJit` if it has one.
    #[inline]
    pub(crate) fn jit_if(&self) -> Option<&'static MethodJit> {
        // SAFETY: published once and never freed.
        unsafe { self.jit.load(Ordering::Acquire).as_ref() }
    }

    /// A method; with `stub`, one whose body the VM supplies (it runs as a native).
    pub(crate) fn new(holder: &'static Klass, name: Symbol, desc: Symbol, access: u16, code: Option<&'static CodeInfo>, stub: bool) -> Method {
        let native = access & crate::klass::ACC_NATIVE != 0 || stub;
        Method {
            holder,
            name,
            desc,
            access,
            sig: MethodSig::parse(desc),
            code,
            vtable_index: AtomicI32::new(-1),
            leaf_native: OnceLock::new(),
            invocations: AtomicU32::new(0),
            backedges: AtomicU32::new(0),
            entry: AtomicUsize::new(if native { crate::jit::native_entry() } else { crate::jit::interpreter_entry() }),
            reg_call: AtomicUsize::new(0),
            jit: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
            stub,
            overridden: crate::low::leak(std::sync::atomic::AtomicBool::new(false)),
            not_compilable: std::sync::atomic::AtomicBool::new(false),
            hidden: OnceLock::new(),
            tiered: std::sync::atomic::AtomicBool::new(false),
            queued: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// A frame stack traces and stack walks leave out (unless asked to show hidden frames): a
    /// method of a hidden class, or one annotated `@Hidden` (method-handle plumbing).
    pub fn is_hidden_frame(&self) -> bool {
        *self.hidden.get_or_init(|| {
            self.holder.is_hidden() || self.has_annotation("Ljava/lang/invoke/LambdaForm$Hidden;") || self.has_annotation("Ljdk/internal/vm/annotation/Hidden;")
        })
    }

    pub fn is_static(&self) -> bool {
        self.access & ACC_STATIC != 0
    }
    pub fn is_overridden(&self) -> bool {
        self.overridden.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Records that a loaded class overrides this method, and sets the flags of the compiled
    /// code that depends on its being unique.
    pub(crate) fn mark_overridden(&self) {
        self.overridden.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(flags) = dependents().lock().unwrap().remove(&(self as *const Method as usize)) {
            for f in flags {
                f.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    /// Makes `flag` (one compilation's dependency byte) set once a class overrides this method.
    pub(crate) fn add_dependent(&self, flag: &'static std::sync::atomic::AtomicBool) {
        {
            let mut all = dependents().lock().unwrap();
            let list = all.entry(self as *const Method as usize).or_default();
            if !list.iter().any(|f| std::ptr::eq(*f, flag)) {
                list.push(flag);
            }
        }
        // Overridden before the registration: `mark_overridden` stored the flag before taking
        // the lock, so this sees it.
        if self.is_overridden() {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Runs native code: declared `native`, or a JDK method whose body the VM supplies.
    pub fn is_native(&self) -> bool {
        self.access & ACC_NATIVE != 0 || self.stub
    }
    pub fn is_abstract(&self) -> bool {
        self.access & ACC_ABSTRACT != 0
    }
    pub fn is_private(&self) -> bool {
        self.access & ACC_PRIVATE != 0
    }
    pub fn is_synchronized(&self) -> bool {
        self.access & ACC_SYNCHRONIZED != 0
    }

    pub(crate) fn compiled(&self) -> Option<&'static crate::jit::CompiledMethod> {
        // SAFETY: published compiled methods are leaked and never freed.
        unsafe { self.jit_if()?.compiled.load(Ordering::Acquire).as_ref() }
    }

    /// The latest baseline code: block entries for exception handlers and on-stack replacement,
    /// and what optimized code is retired to (`retire`).
    pub(crate) fn baseline(&self) -> Option<&'static crate::jit::CompiledMethod> {
        // SAFETY: published compiled methods are leaked and never freed.
        unsafe { self.jit_if()?.baseline.load(Ordering::Acquire).as_ref() }
    }

    /// The newest on-stack-replacement version.
    pub(crate) fn osr_head(&self) -> Option<&'static crate::jit::OsrCode> {
        // SAFETY: published versions are leaked and never freed.
        unsafe { self.jit_if()?.osr.load(Ordering::Acquire).as_ref() }
    }

    /// Publishes `c` as the newest on-stack-replacement version.
    pub(crate) fn push_osr(&self, c: crate::jit::OsrCode) -> &'static crate::jit::OsrCode {
        let c = Box::leak(Box::new(c));
        let osr = &self.jit().osr;
        loop {
            let head = osr.load(Ordering::Acquire);
            // SAFETY: as in `osr_head`.
            c.set_next(unsafe { head.as_ref() });
            if osr.compare_exchange(head, c, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return c;
            }
        }
    }

    /// Publishes new machine code for this method.
    pub(crate) fn install(&self, cm: crate::jit::CompiledMethod) -> &'static crate::jit::CompiledMethod {
        let cm: &'static crate::jit::CompiledMethod = Box::leak(Box::new(cm));
        let jit = self.jit();
        if !cm.optimized {
            jit.baseline.store(cm as *const _ as *mut _, Ordering::Release);
        }
        jit.compiled.store(cm as *const _ as *mut _, Ordering::Release);
        self.entry.store(cm.entry, Ordering::Release);
        crate::jit::code_cache::repoint(self);
        cm
    }

    /// Installs `cm` in place of `current`, unless other code replaced that meanwhile.
    pub(crate) fn replace(&self, current: &'static crate::jit::CompiledMethod, cm: crate::jit::CompiledMethod) -> bool {
        let cm: &'static crate::jit::CompiledMethod = Box::leak(Box::new(cm));
        let old = current as *const _ as *mut _;
        let jit = self.jit();
        if jit.compiled.compare_exchange(old, cm as *const _ as *mut _, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        if !cm.optimized {
            jit.baseline.store(cm as *const _ as *mut _, Ordering::Release);
        }
        let _ = self.entry.compare_exchange(current.entry, cm.entry, Ordering::AcqRel, Ordering::Acquire);
        crate::jit::code_cache::repoint(self);
        true
    }

    /// Replaces `current` (unless newer code replaced it already) by the latest baseline code,
    /// HotSpot's "not entrant": calls stop entering code that keeps deoptimizing while its
    /// recompilation waits. Frames running `current` finish in it.
    pub(crate) fn retire(&self, current: &'static crate::jit::CompiledMethod) {
        let Some(b) = self.baseline() else { return };
        let old = current as *const _ as *mut _;
        if self.jit().compiled.compare_exchange(old, b as *const _ as *mut _, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            let _ = self.entry.compare_exchange(current.entry, b.entry, Ordering::AcqRel, Ordering::Acquire);
            crate::jit::code_cache::repoint(self);
        }
    }

    /// Slots for all arguments including the receiver.
    pub fn param_slots(&self) -> u16 {
        self.sig.arg_slots + if self.is_static() { 0 } else { 1 }
    }

    /// Position of this method in its holder's method table (the reflection "slot").
    pub fn index(&self) -> usize {
        let base = self.holder.methods().as_ptr();
        // SAFETY: `self` is an element of the holder's method slice.
        unsafe { (self as *const Method).offset_from(base) as usize }
    }

    pub fn vtable_index(&self) -> i32 {
        self.vtable_index.load(Ordering::Relaxed)
    }

    pub(crate) fn set_vtable_index(&self, i: i32) {
        self.vtable_index.store(i, Ordering::Relaxed)
    }

    /// Takes part in virtual dispatch (JVMS 5.4.5 overriding candidates).
    pub fn is_virtual(&self) -> bool {
        !self.is_static() && !self.is_private() && self.name.as_str() != "<init>"
    }

    /// Whether the method carries a runtime-visible annotation of type `desc` (e.g.
    /// `Ljdk/internal/vm/annotation/ForceInline;`).
    pub fn has_annotation(&self, desc: &str) -> bool {
        let Some(cf) = self.holder.cf.as_ref() else { return false };
        let info = &cf.methods[self.index()];
        info.attrs.iter().filter(|a| a.name == "RuntimeVisibleAnnotations").any(|a| {
            crate::classfile::annotation_types(cf.attr_bytes(*a)).into_iter().any(|i| cf.utf8(i).is_ok_and(|s| s == desc))
        })
    }

    /// JVMS 2.9.3: a native varargs method of `MethodHandle` or `VarHandle` taking `Object[]`.
    pub fn is_signature_polymorphic(&self) -> bool {
        self.is_native()
            && self.access & 0x0080 != 0
            && (self.holder.name == "java/lang/invoke/MethodHandle" || self.holder.name == "java/lang/invoke/VarHandle")
    }

    pub fn is_default_candidate(&self) -> bool {
        !self.is_static() && !self.is_private() && !self.is_abstract()
    }
}

impl std::fmt::Debug for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}{}", self.holder.name, self.name, self.desc)
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

/// The dependency flags of compiled code, by the method whose uniqueness it assumed.
fn dependents() -> &'static std::sync::Mutex<std::collections::HashMap<usize, Vec<&'static std::sync::atomic::AtomicBool>>> {
    static D: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<usize, Vec<&'static std::sync::atomic::AtomicBool>>>> = std::sync::OnceLock::new();
    D.get_or_init(Default::default)
}
