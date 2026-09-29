//! Runtime method metadata.

use crate::classfile::CodeInfo;
use crate::klass::{Klass, ACC_ABSTRACT, ACC_NATIVE, ACC_PRIVATE, ACC_STATIC, ACC_SYNCHRONIZED};
use crate::natives::NativeFn;
use crate::symbol::Symbol;
use crate::types::MethodSig;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
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
    /// Invocation counter feeding tier-up decisions (generic JVM adaptivity).
    pub invocations: AtomicU32,
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
        }
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
