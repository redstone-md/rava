//! `java.lang.Class` reflection natives: members as `Field`/`Method`/`Constructor` objects and
//! the class-file attributes reflection reads (signatures, annotations, nesting, records).

use super::{ret_bool, ret_int, ret_ref, NativeRegistry};
use crate::classfile::{Attr, ClassFile};
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::method::Method;
use crate::runtime as rt;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::types::{skip_field_desc, BasicType};
use crate::vm::vm;

const C: &str = "java/lang/Class";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(C, "getDeclaredFields0", "(Z)[Ljava/lang/reflect/Field;", declared_fields);
    r.add(C, "getDeclaredMethods0", "(Z)[Ljava/lang/reflect/Method;", |t, a| declared_executables(t, a, false));
    r.add(C, "getDeclaredConstructors0", "(Z)[Ljava/lang/reflect/Constructor;", |t, a| declared_executables(t, a, true));
    r.add(C, "getInterfaces0", "()[Ljava/lang/Class;", |t, a| {
        let ifaces: Vec<&'static Klass> = klass(a[0]).map_or(Vec::new(), |k| k.interfaces.to_vec());
        ret_ref(class_array(t, &ifaces)?)
    });
    r.add(C, "getGenericSignature0", "()Ljava/lang/String;", |t, a| {
        ret_ref(klass(a[0]).and_then(|k| attr_utf8(k, k.cf.as_ref()?.attr("Signature")?)).map_or(Ok(std::ptr::null_mut()), |s| rt::new_string(t, s))?)
    });
    r.add(C, "getRawAnnotations", "()[B", |t, a| {
        ret_ref(klass(a[0]).and_then(|k| k.cf.as_ref()?.attr("RuntimeVisibleAnnotations").map(|at| (k, at))).map_or(Ok(std::ptr::null_mut()), |(k, at)| bytes_of(t, k, at))?)
    });
    r.add(C, "getRawTypeAnnotations", "()[B", |_, _| ret_ref(std::ptr::null_mut()));
    r.add(C, "getConstantPool", "()Ljdk/internal/reflect/ConstantPool;", |t, a| {
        let cp_class = crate::loader::load("jdk/internal/reflect/ConstantPool").expect("ConstantPool");
        rt::initialize(t, cp_class)?;
        let cp = rt::alloc_instance(t, cp_class)?;
        let f = cp_class.declared_field(Symbol::intern("constantPoolOop"), Symbol::intern("Ljava/lang/Object;")).expect("constantPoolOop");
        // SAFETY: fresh ConstantPool.
        unsafe { obj::write_ref(cp, f.offset, a[0] as Oop) };
        ret_ref(cp)
    });
    r.add(C, "isHidden", "()Z", |_, a| ret_bool(klass(a[0]).is_some_and(|k| k.is_hidden())));
    r.add(C, "isRecord0", "()Z", |_, a| {
        ret_bool(klass(a[0]).is_some_and(|k| k.super_klass.is_some_and(|s| s.name == "java/lang/Record") && k.cf.as_ref().is_some_and(|cf| cf.attr("Record").is_some())))
    });
    r.add(C, "getClassAccessFlagsRaw0", "()I", |_, a| ret_int(klass(a[0]).map_or(0x0411, |k| k.access as i32)));
    r.add(C, "getClassFileVersion0", "()I", |_, a| ret_int(klass(a[0]).and_then(|k| k.cf.as_ref()).map_or(65, |cf| cf.major as i32)));
    r.add(C, "getProtectionDomain0", "()Ljava/security/ProtectionDomain;", |_, a| {
        // SAFETY: the receiver, a live mirror.
        ret_ref(unsafe { obj::read_ref(a[0] as Oop, super::classloader::protection_domain()) })
    });
    r.add(C, "getSigners", "()[Ljava/lang/Object;", |_, _| ret_ref(std::ptr::null_mut()));
    r.add(C, "setSigners", "([Ljava/lang/Object;)V", |_, _| Ok(0));
    r.add(C, "getDeclaringClass0", "()Ljava/lang/Class;", |t, a| inner_class_info(t, a[0], InnerQuery::DeclaringClass));
    r.add(C, "getSimpleBinaryName0", "()Ljava/lang/String;", |t, a| inner_class_info(t, a[0], InnerQuery::SimpleName));
    r.add(C, "getDeclaredClasses0", "()[Ljava/lang/Class;", |t, _| ret_ref(class_array(t, &[])?));
    r.add(C, "getEnclosingMethod0", "()[Ljava/lang/Object;", enclosing_method);
    r.add(C, "getNestHost0", "()Ljava/lang/Class;", |t, a| match klass(a[0]) {
        Some(k) => {
            let host = k.nest_host(t);
            ret_ref(rt::mirror_of(t, host)?)
        }
        None => ret_ref(a[0] as Oop),
    });
    r.add(C, "getPermittedSubclasses0", "()[Ljava/lang/Class;", |_, _| ret_ref(std::ptr::null_mut()));
}

pub(super) fn klass(mirror: u64) -> Option<&'static Klass> {
    rt::klass_of_mirror(mirror as Oop)
}

fn attr_utf8(k: &'static Klass, at: Attr) -> Option<&'static str> {
    let cf = k.cf.as_ref()?;
    let b = cf.attr_bytes(at);
    cf.utf8(u16::from_be_bytes([b[0], b[1]])).ok().map(|s| s.as_str())
}

fn bytes_of(t: &mut Thread, k: &'static Klass, at: Attr) -> JResult<Oop> {
    let src = k.cf.as_ref().unwrap().attr_bytes(at);
    let arr = rt::alloc_array(t, vm().primitive_array_class(BasicType::Byte).expect("[B"), src.len() as i32)?;
    // SAFETY: fresh byte[] of the same length.
    unsafe { obj::array_slice::<u8>(arr).copy_from_slice(src) };
    Ok(arr)
}

fn member_attr(attrs: &[Attr], name: &str) -> Option<Attr> {
    attrs.iter().copied().find(|a| a.name == name)
}

/// A fresh `elem[]` of `len` nulls. Build Java arrays first and fill them in place: a Rust
/// `Vec` of references is not a collector root, but the array (held in a local) is.
pub(super) fn new_object_array(t: &mut Thread, elem: &'static Klass, len: usize) -> JResult<Oop> {
    let array_class = vm().array_class_of(elem).map_err(|e| rt::link_error(t, e.into()))?;
    rt::alloc_array(t, array_class, len as i32)
}

pub(super) fn store(array: Oop, i: usize, value: Oop) {
    // SAFETY: `array` is a reference array with more than `i` elements.
    unsafe { obj::store_elem(array, i as usize, value) };
}

/// A `Class[]` of the given classes.
pub(crate) fn class_array(t: &mut Thread, classes: &[&'static Klass]) -> JResult<Oop> {
    let arr = new_object_array(t, crate::loader::load(C).expect("Class"), classes.len())?;
    for (i, &k) in classes.iter().enumerate() {
        let m = rt::mirror_of(t, k)?;
        store(arr, i, m);
    }
    Ok(arr)
}

/// Parameter mirrors and return mirror of a method descriptor.
pub(crate) fn signature_mirrors(t: &mut Thread, m: &'static Method) -> JResult<(Oop, Oop)> {
    let d = m.desc.as_str();
    let b = d.as_bytes();
    let params = new_object_array(t, crate::loader::load(C).expect("Class"), m.sig.params.len())?;
    let (mut i, mut n) = (1, 0);
    while b[i] != b')' {
        let end = skip_field_desc(b, i);
        let p = rt::mirror_for_desc(t, m.holder, &d[i..end])?;
        store(params, n, p);
        (i, n) = (end, n + 1);
    }
    let ret = rt::mirror_for_desc(t, m.holder, &d[i + 1..])?;
    Ok((params, ret))
}

fn exception_mirrors(t: &mut Thread, m: &'static Method) -> JResult<Oop> {
    let cf = m.holder.cf.as_ref().unwrap();
    let info = &cf.methods[m.index()];
    let indices: Vec<u16> = member_attr(&info.attrs, "Exceptions").map_or(Vec::new(), |at| {
        let b = cf.attr_bytes(at);
        (0..u16::from_be_bytes([b[0], b[1]]) as usize).map(|i| u16::from_be_bytes([b[2 + 2 * i], b[3 + 2 * i]])).collect()
    });
    let arr = new_object_array(t, crate::loader::load(C).expect("Class"), indices.len())?;
    for (i, idx) in indices.into_iter().enumerate() {
        let k = m.holder.resolve_class(t, idx).map_err(|e| rt::link_error(t, e))?;
        let mirror = rt::mirror_of(t, k)?;
        store(arr, i, mirror);
    }
    Ok(arr)
}

pub(super) fn optional_string(t: &mut Thread, s: Option<&'static str>) -> JResult<Oop> {
    s.map_or(Ok(std::ptr::null_mut()), |s| rt::new_string(t, s))
}

fn optional_bytes(t: &mut Thread, k: &'static Klass, at: Option<Attr>) -> JResult<Oop> {
    at.map_or(Ok(std::ptr::null_mut()), |at| bytes_of(t, k, at))
}

fn declared_fields(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let field_class = crate::loader::load("java/lang/reflect/Field").expect("Field");
    rt::initialize(t, field_class)?;
    let Some(k) = klass(a[0]) else { return ret_ref(new_object_array(t, field_class, 0)?) };
    let public_only = a[1] != 0;
    let cf = k.cf.as_ref();
    // Injected VM fields sit past the class file's own fields and stay invisible.
    let slots: Vec<usize> = (0..cf.map_or(0, |cf| cf.fields.len()))
        .filter(|&i| !public_only || k.fields[i].access & 1 != 0)
        .collect();
    let out = new_object_array(t, field_class, slots.len())?;
    for (n, slot) in slots.into_iter().enumerate() {
        let (f, info) = (&k.fields[slot], &cf.unwrap().fields[slot]);
        let name = rt::intern_str(t, f.name.as_str())?;
        let ty = rt::mirror_for_desc(t, k, f.desc.as_str())?;
        let sig = optional_string(t, member_attr(&info.attrs, "Signature").and_then(|at| attr_utf8(k, at)))?;
        let ann = optional_bytes(t, k, member_attr(&info.attrs, "RuntimeVisibleAnnotations"))?;
        let trusted_final = f.access & 0x0010 != 0 && (f.is_static() || k.is_hidden() || k.super_klass.is_some_and(|s| s.name == "java/lang/Record"));
        let field = crate::boot::construct(
            t,
            "java/lang/reflect/Field",
            "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/Class;IZILjava/lang/String;[B)V",
            &[a[0], name as u64, ty as u64, (f.access & 0x50df) as u64, trusted_final as u64, slot as u64, sig as u64, ann as u64],
        )?;
        store(out, n, field);
    }
    ret_ref(out)
}

fn declared_executables(t: &mut Thread, a: &[u64], constructors: bool) -> JResult<u64> {
    let class_name = if constructors { "java/lang/reflect/Constructor" } else { "java/lang/reflect/Method" };
    let exec_class = crate::loader::load(class_name).expect("reflect class");
    rt::initialize(t, exec_class)?;
    let Some(k) = klass(a[0]).filter(|k| !k.is_array()) else { return ret_ref(new_object_array(t, exec_class, 0)?) };
    let public_only = a[1] != 0;
    let slots: Vec<usize> = k
        .methods()
        .iter()
        .enumerate()
        .filter(|(_, m)| (m.name == "<init>") == constructors && m.name != "<clinit>" && (!public_only || m.access & 1 != 0))
        .map(|(i, _)| i)
        .collect();
    let out = new_object_array(t, exec_class, slots.len())?;
    for (n, slot) in slots.into_iter().enumerate() {
        let o = executable_object(t, a[0], k, slot, constructors)?;
        store(out, n, o);
    }
    ret_ref(out)
}

/// The `Method` or `Constructor` object for `k`'s method `slot` (`mirror` is `k`'s mirror).
pub(super) fn executable_object(t: &mut Thread, mirror: u64, k: &'static Klass, slot: usize, constructors: bool) -> JResult<Oop> {
    let (m, info) = (&k.methods()[slot], &k.cf.as_ref().unwrap().methods[slot]);
    let (params, ret) = signature_mirrors(t, m)?;
    let exceptions = exception_mirrors(t, m)?;
    let sig = optional_string(t, member_attr(&info.attrs, "Signature").and_then(|at| attr_utf8(k, at)))?;
    let ann = optional_bytes(t, k, member_attr(&info.attrs, "RuntimeVisibleAnnotations"))?;
    let pann = optional_bytes(t, k, member_attr(&info.attrs, "RuntimeVisibleParameterAnnotations"))?;
    let mods = (m.access & 0x1dff) as u64;
    if constructors {
        return crate::boot::construct(
            t,
            "java/lang/reflect/Constructor",
            "(Ljava/lang/Class;[Ljava/lang/Class;[Ljava/lang/Class;IILjava/lang/String;[B[B)V",
            &[mirror, params as u64, exceptions as u64, mods, slot as u64, sig as u64, ann as u64, pann as u64],
        );
    }
    let name = rt::intern_str(t, m.name.as_str())?;
    let dflt = optional_bytes(t, k, member_attr(&info.attrs, "AnnotationDefault"))?;
    crate::boot::construct(
        t,
        "java/lang/reflect/Method",
        "(Ljava/lang/Class;Ljava/lang/String;[Ljava/lang/Class;Ljava/lang/Class;[Ljava/lang/Class;IILjava/lang/String;[B[B[B)V",
        &[mirror, name as u64, params as u64, ret as u64, exceptions as u64, mods, slot as u64, sig as u64, ann as u64, pann as u64, dflt as u64],
    )
}

enum InnerQuery {
    DeclaringClass,
    SimpleName,
}

/// Answers from the `InnerClasses` entry describing this class itself.
fn inner_class_info(t: &mut Thread, mirror: u64, q: InnerQuery) -> JResult<u64> {
    let Some(k) = klass(mirror) else { return ret_ref(std::ptr::null_mut()) };
    let Some(cf) = k.cf.as_ref() else { return ret_ref(std::ptr::null_mut()) };
    let Some((outer, name)) = own_inner_entry(cf) else { return ret_ref(std::ptr::null_mut()) };
    match q {
        InnerQuery::DeclaringClass if outer != 0 => {
            let o = k.resolve_class(t, outer).map_err(|e| rt::link_error(t, e))?;
            ret_ref(rt::mirror_of(t, o)?)
        }
        InnerQuery::SimpleName if name != 0 => ret_ref(optional_string(t, cf.utf8(name).ok().map(|s| s.as_str()))?),
        _ => ret_ref(std::ptr::null_mut()),
    }
}

/// `(outer_class_info_index, inner_name_index)` of the InnerClasses entry naming this class.
fn own_inner_entry(cf: &ClassFile) -> Option<(u16, u16)> {
    let b = cf.attr_bytes(cf.attr("InnerClasses")?);
    let n = u16::from_be_bytes([b[0], b[1]]) as usize;
    (0..n).map(|i| &b[2 + 8 * i..10 + 8 * i]).find_map(|e| {
        let inner = u16::from_be_bytes([e[0], e[1]]);
        (cf.class_name(inner) == Some(cf.name)).then(|| (u16::from_be_bytes([e[2], e[3]]), u16::from_be_bytes([e[4], e[5]])))
    })
}

fn enclosing_method(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let Some(k) = klass(a[0]) else { return ret_ref(std::ptr::null_mut()) };
    let Some(cf) = k.cf.as_ref() else { return ret_ref(std::ptr::null_mut()) };
    let Some(at) = cf.attr("EnclosingMethod") else { return ret_ref(std::ptr::null_mut()) };
    let b = cf.attr_bytes(at);
    let (class_idx, nat) = (u16::from_be_bytes([b[0], b[1]]), u16::from_be_bytes([b[2], b[3]]));
    let outer = k.resolve_class(t, class_idx).map_err(|e| rt::link_error(t, e))?;
    let items = new_object_array(t, crate::loader::load("java/lang/Object").expect("Object"), 3)?;
    let outer_mirror = rt::mirror_of(t, outer)?;
    store(items, 0, outer_mirror);
    if let Some((name, desc)) = cf.name_and_type(nat) {
        let n = rt::new_string(t, name.as_str())?;
        store(items, 1, n);
        let d = rt::new_string(t, desc.as_str())?;
        store(items, 2, d);
    }
    ret_ref(items)
}

