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
    /// Index in the holder's (and every subclass's) vtable; -1 when not virtually dispatched.
    vtable_index: AtomicI32,
    pub(crate) native: OnceLock<NativeFn>,
    /// Invocation and loop back-edge counters feeding tier-up and on-stack replacement
    /// (generic JVM adaptivity). Racy increments are acceptable: they only time the tier-up.
    pub invocations: AtomicU32,
    pub backedges: AtomicU32,
    /// Machine-code entry with the compiled-code ABI (`jit::EntryFn`): compiled code, or an
    /// adapter into the interpreter / native. Generated code calls through this field.
    pub(crate) entry: AtomicUsize,
    /// Current compiled code (replaced on recompilation; old code stays valid for frames still
    /// running it, since every version shares the frame layout).
    compiled: std::sync::atomic::AtomicPtr<crate::jit::CompiledMethod>,
    pub(crate) recompiles: AtomicU32,
    /// Recompilations of optimized code after deoptimizations (`jit/opt`).
    pub(crate) deopt_recompiles: AtomicU32,
    /// Set once compilation was attempted and refused, so it is not retried.
    pub(crate) not_compilable: std::sync::atomic::AtomicBool,
    hidden: OnceLock<bool>,
    /// Set once tier-2 compilation was requested.
    pub(crate) tiered: std::sync::atomic::AtomicBool,
}

impl Method {
    pub(crate) fn new(holder: &'static Klass, name: Symbol, desc: Symbol, access: u16, code: Option<&'static CodeInfo>) -> Method {
        Method {
            holder,
            name,
            desc,
            access,
            sig: MethodSig::parse(desc),
            code,
            vtable_index: AtomicI32::new(-1),
            native: OnceLock::new(),
            invocations: AtomicU32::new(0),
            backedges: AtomicU32::new(0),
            entry: AtomicUsize::new(crate::jit::interpreter_entry()),
            compiled: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
            recompiles: AtomicU32::new(0),
            deopt_recompiles: AtomicU32::new(0),
            not_compilable: std::sync::atomic::AtomicBool::new(false),
            hidden: OnceLock::new(),
            tiered: std::sync::atomic::AtomicBool::new(false),
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
    pub fn is_native(&self) -> bool {
        self.access & ACC_NATIVE != 0
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
        unsafe { self.compiled.load(Ordering::Acquire).as_ref() }
    }

    /// Publishes new machine code for this method.
    pub(crate) fn install(&self, cm: crate::jit::CompiledMethod) -> &'static crate::jit::CompiledMethod {
        let cm: &'static crate::jit::CompiledMethod = Box::leak(Box::new(cm));
        self.compiled.store(cm as *const _ as *mut _, Ordering::Release);
        self.entry.store(cm.entry, Ordering::Release);
        cm
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
