//! Constant-pool resolution (JVMS 5.4.3), cached per class and pool index.

use crate::classfile::Cp;
use crate::klass::{FieldDesc, Klass, ACC_STATIC};
use crate::loader::LoadError;
use crate::method::Method;
use crate::symbol::Symbol;
use crate::thread::Thread;
use crate::types::BasicType;
use crate::vm::vm;
use std::sync::atomic::AtomicU64;

#[derive(Clone, Copy, Debug)]
pub enum Resolved {
    Class(&'static Klass),
    InstanceField { offset: u32, ty: BasicType, volatile: bool },
    StaticField { holder: &'static Klass, slot: &'static AtomicU64, ty: BasicType },
    Method(&'static Method),
    /// An interned `java.lang.String` literal.
    String(usize),
    /// A resolved object constant (method type, method handle, dynamic constant).
    Constant(usize),
}

// Resolved entries only name immortal metadata and immortal interned strings.
unsafe impl Send for Resolved {}
unsafe impl Sync for Resolved {}

#[derive(Debug)]
pub enum LinkError {
    Load(LoadError),
    /// (error class, message), e.g. `java/lang/NoSuchFieldError`.
    Error(&'static str, String),
    /// A Java exception raised while loading (e.g. by a class loader's `loadClass`).
    Thrown(crate::thread::Throw),
}


pub type LinkResult<T> = Result<T, LinkError>;

impl Klass {
    /// Class constant `index` of this class's pool, resolved and cached.
    pub fn resolve_class(&'static self, t: &mut Thread, index: u16) -> LinkResult<&'static Klass> {
        if let Some(Resolved::Class(k)) = self.resolved[index as usize].get() {
            return Ok(k);
        }
        let cf = self.cf.as_ref().unwrap();
        let name = cf.class_name(index).ok_or_else(|| bad_pool(self, index))?;
        // A class's references to its own name denote itself (hidden classes are not findable).
        let k = if name == cf.name { self } else { vm().load_class(t, self.loader(), name)? };
        let _ = self.resolved[index as usize].set(Resolved::Class(k));
        Ok(k)
    }

    /// The nest host (JVMS 5.4.4): fixed for hidden nestmates, else the `NestHost` attribute's
    /// class, else the class itself.
    pub fn nest_host(&'static self, t: &mut Thread) -> &'static Klass {
        if let Some(h) = self.nest_host.get() {
            return h;
        }
        let declared = self.cf.as_ref().and_then(|cf| {
            let b = cf.attr_bytes(cf.attr("NestHost")?);
            Some(u16::from_be_bytes([b[0], b[1]]))
        });
        let host = declared.and_then(|i| self.resolve_class(t, i).ok()).unwrap_or(self);
        self.nest_host.get_or_init(|| host)
    }

    /// Field reference `index`, resolved to its storage.
    pub fn resolve_field(&'static self, t: &mut Thread, index: u16) -> LinkResult<Resolved> {
        if let Some(r) = self.resolved[index as usize].get() {
            return Ok(*r);
        }
        let cf = self.cf.as_ref().unwrap();
        let (_, name, desc) = cf.member_ref(index).ok_or_else(|| bad_pool(self, index))?;
        let class_index = match cf.pool[index as usize] {
            Cp::Field { class, .. } => class,
            _ => return Err(bad_pool(self, index)),
        };
        let owner = self.resolve_class(t, class_index)?;
        let (holder, f) = find_field(owner, name, desc)
            .ok_or_else(|| LinkError::Error("java/lang/NoSuchFieldError", name.to_string()))?;
        let r = if f.access & ACC_STATIC != 0 {
            Resolved::StaticField { holder, slot: &holder.statics[f.offset as usize], ty: f.ty }
        } else {
            Resolved::InstanceField { offset: f.offset, ty: f.ty, volatile: f.access & 0x0040 != 0 }
        };
        Ok(*self.resolved[index as usize].get_or_init(|| r))
    }

    /// Method or interface-method reference `index`, resolved to the declared target
    /// (JVMS 5.4.3.3 / 5.4.3.4). Dispatch happens at the call site.
    pub fn resolve_method(&'static self, t: &mut Thread, index: u16) -> LinkResult<&'static Method> {
        if let Some(Resolved::Method(m)) = self.resolved[index as usize].get() {
            return Ok(m);
        }
        let cf = self.cf.as_ref().unwrap();
        let (_, name, desc) = cf.member_ref(index).ok_or_else(|| bad_pool(self, index))?;
        let class_index = match cf.pool[index as usize] {
            Cp::Method { class, .. } | Cp::IMethod { class, .. } => class,
            _ => return Err(bad_pool(self, index)),
        };
        let owner = self.resolve_class(t, class_index)?;
        let m = lookup_method(owner, name, desc).ok_or_else(|| {
            LinkError::Error("java/lang/NoSuchMethodError", format!("'{desc} {}.{name}'", owner.name))
        })?;
        let _ = self.resolved[index as usize].set(Resolved::Method(m));
        Ok(m)
    }
}

fn bad_pool(k: &Klass, index: u16) -> LinkError {
    LinkError::Error("java/lang/IncompatibleClassChangeError", format!("{}: bad constant #{index}", k.name))
}

/// JVMS 5.4.3.2: the class, then its superinterfaces, then its superclass.
pub fn find_field(k: &'static Klass, name: Symbol, desc: Symbol) -> Option<(&'static Klass, &'static FieldDesc)> {
    if let Some(f) = k.declared_field(name, desc) {
        return Some((k, f));
    }
    for &i in k.interfaces.iter() {
        if let Some(found) = find_field(i, name, desc) {
            return Some(found);
        }
    }
    find_field(k.super_klass?, name, desc)
}

/// Method resolution: the class and its superclasses, then superinterfaces (preferring a
/// non-abstract, maximally-specific default), for classes and interfaces alike.
pub fn lookup_method(owner: &'static Klass, name: Symbol, desc: Symbol) -> Option<&'static Method> {
    if let Some(m) = owner.supers().find_map(|k| k.declared_method(name, desc)) {
        return Some(m);
    }
    if let Some(m) = signature_polymorphic(owner, name) {
        return Some(m);
    }
    let candidates = owner.all_interfaces.iter().filter_map(|i| i.declared_method(name, desc));
    let mut abstract_hit = None;
    for m in candidates.filter(|m| !m.is_static() && !m.is_private()) {
        if !m.is_abstract() {
            return Some(m);
        }
        abstract_hit.get_or_insert(m);
    }
    abstract_hit
}

/// `MethodHandle.invoke*` / `VarHandle` accessors accept any descriptor (JVMS 2.9.3).
fn signature_polymorphic(owner: &'static Klass, name: Symbol) -> Option<&'static Method> {
    let n = owner.name.as_str();
    if n != "java/lang/invoke/MethodHandle" && n != "java/lang/invoke/VarHandle" {
        return None;
    }
    owner.methods().iter().find(|m| m.name == name && m.is_native() && m.access & 0x0080 != 0)
}

/// Selects the implementation of `m` for a receiver of class `recv` (invokevirtual /
/// invokeinterface, JVMS 5.4.6).
pub fn select_method(recv: &'static Klass, m: &'static Method) -> Option<&'static Method> {
    let vi = m.vtable_index();
    if vi >= 0 && !m.holder.is_interface() {
        return recv.vtable().get(vi as usize).copied();
    }
    if m.holder.is_interface() && !recv.is_interface() {
        let itable = itable_of(recv);
        let iface = m.holder as *const Klass as usize;
        if let Some((_, block)) = itable.iter().find(|e| e.0 == iface) {
            return block[m.index()];
        }
    }
    find_implementation(recv, m)
}

/// `k`'s itable, built on first use and then also published flat for generated code.
pub(crate) fn itable_of(k: &'static Klass) -> &'static [crate::klass::Itable] {
    let itable = k.itable.get_or_init(|| build_itable(k));
    if k.itable_raw.load(std::sync::atomic::Ordering::Acquire).is_null() {
        let mut raw = vec![itable.len()];
        for (iface, block) in itable.iter() {
            raw.extend([*iface, block.as_ptr() as usize]);
        }
        let raw = Box::leak(raw.into_boxed_slice());
        k.itable_raw.store(raw.as_mut_ptr(), std::sync::atomic::Ordering::Release);
    }
    itable
}

/// For every interface `k` implements, the implementation of each of its methods (`None` for
/// static, private or still-abstract ones). Built once per class; lookups are lock-free.
fn build_itable(k: &'static Klass) -> Box<[crate::klass::Itable]> {
    k.all_interfaces
        .iter()
        .map(|i| {
            let block = i
                .methods()
                .iter()
                .map(|im| (!im.is_static() && !im.is_private() && im.name.as_str() != "<clinit>").then(|| find_implementation(k, im)).flatten())
                .collect();
            (*i as *const Klass as usize, block)
        })
        .collect()
}

/// The non-abstract method `recv` uses for `m`'s name and descriptor: its vtable (which holds
/// inherited defaults), else a maximally-specific default method.
fn find_implementation(recv: &'static Klass, m: &'static Method) -> Option<&'static Method> {
    recv.vtable()
        .iter()
        .rev()
        .copied()
        .find(|c| c.name == m.name && c.desc == m.desc && !c.is_abstract())
        .or_else(|| lookup_method(recv, m.name, m.desc).filter(|c| !c.is_abstract()))
}
