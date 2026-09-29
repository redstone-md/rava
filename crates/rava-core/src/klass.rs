//! Runtime class metadata. A `Klass` is immortal (`&'static`) once defined, so compiled code and
//! object headers can point at it directly.

use crate::classfile::ClassFile;
use crate::heap::Oop;
use crate::link::Resolved;
use crate::method::Method;
use crate::symbol::Symbol;
use crate::types::BasicType;
use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread::ThreadId;

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_PROTECTED: u16 = 0x0004;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_FINAL: u16 = 0x0010;
pub const ACC_SYNCHRONIZED: u16 = 0x0020;
pub const ACC_NATIVE: u16 = 0x0100;
pub const ACC_INTERFACE: u16 = 0x0200;
pub const ACC_ABSTRACT: u16 = 0x0400;

pub enum KlassKind {
    Instance,
    ObjArray { elem: &'static Klass },
    TypeArray { elem: BasicType },
}

/// A declared field with its resolved storage: a byte offset for instance fields, a slot index
/// into `Klass::statics` for static ones.
#[derive(Debug)]
pub struct FieldDesc {
    pub name: Symbol,
    pub desc: Symbol,
    pub access: u16,
    pub ty: BasicType,
    pub offset: u32,
    pub constant_value: u16,
}

impl FieldDesc {
    pub fn is_static(&self) -> bool {
        self.access & ACC_STATIC != 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum InitState {
    Linked = 0,
    BeingInitialized = 1,
    Initialized = 2,
    Erroneous = 3,
}

/// JVMS 5.5 initialization lock.
pub struct InitLock {
    state: AtomicU8,
    pub(crate) owner: Mutex<Option<ThreadId>>,
    pub(crate) cv: Condvar,
}

impl InitLock {
    fn new(state: InitState) -> Self {
        Self { state: AtomicU8::new(state as u8), owner: Mutex::new(None), cv: Condvar::new() }
    }

    #[inline(always)]
    pub fn state(&self) -> InitState {
        match self.state.load(Ordering::Acquire) {
            0 => InitState::Linked,
            1 => InitState::BeingInitialized,
            2 => InitState::Initialized,
            _ => InitState::Erroneous,
        }
    }

    pub(crate) fn set(&self, s: InitState) {
        self.state.store(s as u8, Ordering::Release)
    }
}

pub struct Klass {
    pub name: Symbol,
    pub kind: KlassKind,
    pub access: u16,
    pub super_klass: Option<&'static Klass>,
    pub interfaces: Box<[&'static Klass]>,
    /// Every interface this class implements, transitively (for subtype checks).
    pub all_interfaces: Box<[&'static Klass]>,
    pub cf: Option<ClassFile>,
    pub fields: Box<[FieldDesc]>,
    instance_size: u32,
    /// Offsets of every reference field of an instance, inherited ones included.
    pub ref_offsets: Box<[u32]>,
    pub statics: Box<[AtomicU64]>,
    pub(crate) methods: OnceLock<Box<[Method]>>,
    pub(crate) vtable: OnceLock<Box<[&'static Method]>>,
    pub init: InitLock,
    pub(crate) mirror: AtomicPtr<crate::heap::ObjHeader>,
    pub(crate) loader: AtomicPtr<crate::heap::ObjHeader>,
    pub(crate) resolved: Box<[OnceLock<Resolved>]>,
    pub(crate) array_klass: OnceLock<&'static Klass>,
    pub(crate) iface_cache: Mutex<HashMap<(Symbol, Symbol), Option<&'static Method>>>,
    /// Defined by `Lookup.defineHiddenClass` (not findable by name).
    hidden: bool,
}

pub(crate) struct KlassParts {
    pub name: Symbol,
    pub kind: KlassKind,
    pub access: u16,
    pub super_klass: Option<&'static Klass>,
    pub interfaces: Vec<&'static Klass>,
    pub cf: Option<ClassFile>,
    pub fields: Vec<FieldDesc>,
    pub instance_size: u32,
    pub ref_offsets: Vec<u32>,
    pub static_slots: usize,
    pub initialized: bool,
    pub hidden: bool,
}

impl Klass {
    pub(crate) fn new(p: KlassParts) -> Klass {
        let mut all: Vec<&'static Klass> = p.super_klass.map_or(Vec::new(), |s| s.all_interfaces.to_vec());
        for &i in &p.interfaces {
            for &k in std::iter::once(&i).chain(i.all_interfaces.iter()) {
                if !all.iter().any(|x| std::ptr::eq(*x, k)) {
                    all.push(k);
                }
            }
        }
        let pool_len = p.cf.as_ref().map_or(0, |cf| cf.pool.len());
        Klass {
            name: p.name,
            kind: p.kind,
            access: p.access,
            super_klass: p.super_klass,
            interfaces: p.interfaces.into_boxed_slice(),
            all_interfaces: all.into_boxed_slice(),
            cf: p.cf,
            fields: p.fields.into_boxed_slice(),
            instance_size: p.instance_size,
            ref_offsets: p.ref_offsets.into_boxed_slice(),
            statics: (0..p.static_slots).map(|_| AtomicU64::new(0)).collect(),
            methods: OnceLock::new(),
            vtable: OnceLock::new(),
            init: InitLock::new(if p.initialized { InitState::Initialized } else { InitState::Linked }),
            mirror: AtomicPtr::new(std::ptr::null_mut()),
            loader: AtomicPtr::new(std::ptr::null_mut()),
            resolved: (0..pool_len).map(|_| OnceLock::new()).collect(),
            array_klass: OnceLock::new(),
            iface_cache: Mutex::new(HashMap::new()),
            hidden: p.hidden,
        }
    }

    #[inline(always)]
    pub fn instance_size(&self) -> u32 {
        self.instance_size
    }

    pub fn methods(&'static self) -> &'static [Method] {
        self.methods.get().map_or(&[], |m| m)
    }

    pub fn vtable(&'static self) -> &'static [&'static Method] {
        self.vtable.get().map_or(&[], |v| v)
    }

    pub fn is_interface(&self) -> bool {
        self.access & ACC_INTERFACE != 0
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    pub fn is_array(&self) -> bool {
        !matches!(self.kind, KlassKind::Instance)
    }

    pub fn mirror(&self) -> Oop {
        self.mirror.load(Ordering::Acquire)
    }

    pub fn loader(&self) -> Oop {
        self.loader.load(Ordering::Acquire)
    }

    /// Element size in bytes of an array class.
    pub fn elem_size(&self) -> u32 {
        match self.kind {
            KlassKind::TypeArray { elem } => elem.size(),
            _ => 8,
        }
    }

    pub fn package(&self) -> &'static str {
        let n = self.name.as_str();
        n.rsplit_once('/').map_or("", |(p, _)| p)
    }

    /// The method declared by this class with this name and descriptor.
    pub fn declared_method(&'static self, name: Symbol, desc: Symbol) -> Option<&'static Method> {
        self.methods().iter().find(|m| m.name == name && m.desc == desc)
    }

    pub fn declared_field(&self, name: Symbol, desc: Symbol) -> Option<&FieldDesc> {
        self.fields.iter().find(|f| f.name == name && f.desc == desc)
    }

    /// Superclass chain, starting with self.
    pub fn supers(&'static self) -> impl Iterator<Item = &'static Klass> {
        std::iter::successors(Some(self), |k| k.super_klass)
    }

    /// `self` is assignable to `target` (JVMS checkcast/instanceof rules).
    pub fn is_subtype_of(&'static self, target: &'static Klass) -> bool {
        if std::ptr::eq(self, target) {
            return true;
        }
        match (&self.kind, &target.kind) {
            (KlassKind::ObjArray { elem: a }, KlassKind::ObjArray { elem: b }) => a.is_subtype_of(b),
            (KlassKind::TypeArray { .. }, KlassKind::TypeArray { .. }) => false,
            _ if target.is_interface() => self.all_interfaces.iter().any(|i| std::ptr::eq(*i, target)),
            _ => self.supers().any(|k| std::ptr::eq(k, target)),
        }
    }
}

impl std::fmt::Debug for Klass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Klass({})", self.name)
    }
}
