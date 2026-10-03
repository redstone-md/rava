//! Runtime class metadata. A `Klass` is immortal (`&'static`) once defined, so compiled code and
//! object headers can point at it directly.

use crate::classfile::ClassFile;
use crate::heap::Oop;
use crate::link::Resolved;
use crate::method::Method;
use crate::symbol::Symbol;
use crate::types::BasicType;
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
    /// The `Thread` running the initializers while `BeingInitialized` (0 otherwise), so compiled
    /// code of that thread can use the class as initialized (JVMS 5.5 step 3).
    initializer: std::sync::atomic::AtomicUsize,
    pub(crate) owner: Mutex<Option<ThreadId>>,
    pub(crate) cv: Condvar,
}

impl InitLock {
    fn new(state: InitState) -> Self {
        Self { state: AtomicU8::new(state as u8), initializer: Default::default(), owner: Mutex::new(None), cv: Condvar::new() }
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

    /// Byte offset of the state byte, read by generated code.
    pub(crate) const fn state_offset() -> usize {
        std::mem::offset_of!(InitLock, state)
    }

    pub(crate) fn set(&self, s: InitState) {
        self.state.store(s as u8, Ordering::Release)
    }

    /// Byte offset of the initializing thread, read by generated code.
    pub(crate) const fn initializer_offset() -> usize {
        std::mem::offset_of!(InitLock, initializer)
    }

    pub(crate) fn set_initializer(&self, t: usize) {
        self.initializer.store(t, Ordering::Relaxed)
    }
}

/// One interface's block of a class's itable.
pub(crate) type Itable = (usize, Box<[Option<&'static Method>]>);

/// Laid out in declaration order: the fields generated code reads on dispatch and type checks
/// come first, so they share the first cache lines.
#[repr(C)]
pub struct Klass {
    /// Raw views read by generated code: the vtable's first entry, and an object array's
    /// element class (null otherwise).
    pub(crate) vtable_ptr: AtomicPtr<&'static Method>,
    pub(crate) elem_klass: *const Klass,
    /// Superclass chain length (Object: 0) and the ancestors at depths below `PRIMARY` (HotSpot's
    /// primary supers display): a class at depth d below `PRIMARY` is a superclass of an object's
    /// class exactly when it is that class or its `primary[d]`.
    pub(crate) depth: u32,
    pub(crate) primary: [*const Klass; PRIMARY],
    /// The same itable flattened for generated code: `[count, (interface, block pointer)...]`,
    /// or null until first built (`jit/stubs.rs` scans it).
    pub(crate) itable_raw: AtomicPtr<usize>,
    /// The `(interface, block pointer)` pair of `itable_raw` an inline scan found last (HotSpot's
    /// secondary super cache), checked before scanning; a pair of zeros until then.
    pub(crate) itable_hit: AtomicPtr<usize>,
    pub(crate) mirror: AtomicPtr<crate::heap::ObjHeader>,
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
    /// In low memory (`crate::low`): code stores and passes methods as 32-bit immediates.
    pub(crate) methods: OnceLock<&'static [Method]>,
    pub(crate) vtable: OnceLock<Box<[&'static Method]>>,
    pub init: InitLock,
    pub(crate) loader: AtomicPtr<crate::heap::ObjHeader>,
    pub(crate) resolved: Box<[OnceLock<Resolved>]>,
    pub(crate) array_klass: OnceLock<&'static Klass>,
    /// Interface method → implementation, sorted by method address (see `link::select_method`).
    /// Per implemented interface: its class and, by the interface method's index, the
    /// implementation (as HotSpot's itable: a short scan, then an indexed load).
    pub(crate) itable: OnceLock<Box<[Itable]>>,
    /// Defined by `Lookup.defineHiddenClass` (not findable by name).
    hidden: bool,
    /// Nest host fixed at definition (hidden nestmates); otherwise from `NestHost`.
    pub(crate) nest_host: OnceLock<&'static Klass>,
}

/// Depths the primary supers display covers.
pub(crate) const PRIMARY: usize = 8;

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

/// The `itable_hit` of a class no scan has hit yet: no interface is null.
static NO_ITABLE_HIT: [usize; 2] = [0, 0];

/// Classes live in low memory (`crate::low`): generated code compares and stores class pointers
/// as 32-bit immediates.
pub(crate) fn leak(k: Klass) -> &'static Klass {
    crate::low::leak(k)
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
        let (depth, primary) = match p.super_klass {
            Some(sup) => {
                let mut primary = sup.primary;
                if (sup.depth as usize) < PRIMARY {
                    primary[sup.depth as usize] = sup;
                }
                (sup.depth + 1, primary)
            }
            None => (0, [std::ptr::null(); PRIMARY]),
        };
        Klass {
            depth,
            primary,
            name: p.name,
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
            itable: OnceLock::new(),
            itable_raw: AtomicPtr::new(std::ptr::null_mut()),
            itable_hit: AtomicPtr::new(NO_ITABLE_HIT.as_ptr() as *mut usize),
            hidden: p.hidden,
            nest_host: OnceLock::new(),
            vtable_ptr: AtomicPtr::new(std::ptr::null_mut()),
            elem_klass: match p.kind {
                KlassKind::ObjArray { elem } => elem,
                _ => std::ptr::null(),
            },
            kind: p.kind,
        }
    }

    #[inline(always)]
    pub fn instance_size(&self) -> u32 {
        self.instance_size
    }

    pub fn methods(&'static self) -> &'static [Method] {
        self.methods.get().map_or(&[], |m| m)
    }

    pub(crate) fn set_vtable(&'static self, vt: Box<[&'static Method]>) {
        let vt = self.vtable.get_or_init(|| vt);
        self.vtable_ptr.store(vt.as_ptr() as *mut &'static Method, Ordering::Release);
    }

    pub fn vtable(&'static self) -> &'static [&'static Method] {
        self.vtable.get().map_or(&[], |v| v)
    }

    pub fn is_interface(&self) -> bool {
        self.access & ACC_INTERFACE != 0
    }

    /// The binary name as `Class.getName` reports it (`a.b.C`, hidden: `a.b.C/0x...`).
    pub fn java_name(&self) -> String {
        match (self.hidden, self.cf.as_ref()) {
            (true, Some(cf)) => format!("{}{}", cf.name.as_str().replace('/', "."), &self.name.as_str()[cf.name.as_str().len()..]),
            _ => self.name.as_str().replace('/', "."),
        }
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
        // Hidden classes keep their original name in the class file; the suffix is not a package.
        let n = self.cf.as_ref().map_or(self.name, |cf| cf.name).as_str();
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
