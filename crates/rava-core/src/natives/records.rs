//! `Class.getRecordComponents0`: one `RecordComponent` per entry of the `Record` attribute
//! (JVMS 4.7.30), as HotSpot's `Reflection::new_record_component` builds them.

use super::byte_array;
use super::reflect_class::{executable_object, klass, new_object_array, optional_string, store};
use super::{ret_ref, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};

const RC: &str = "java/lang/reflect/RecordComponent";

vm_field!(rc_clazz, "java/lang/reflect/RecordComponent", "clazz", "Ljava/lang/Class;");
vm_field!(rc_name, "java/lang/reflect/RecordComponent", "name", "Ljava/lang/String;");
vm_field!(rc_type, "java/lang/reflect/RecordComponent", "type", "Ljava/lang/Class;");
vm_field!(rc_accessor, "java/lang/reflect/RecordComponent", "accessor", "Ljava/lang/reflect/Method;");
vm_field!(rc_signature, "java/lang/reflect/RecordComponent", "signature", "Ljava/lang/String;");
vm_field!(rc_annotations, "java/lang/reflect/RecordComponent", "annotations", "[B");
vm_field!(rc_type_annotations, "java/lang/reflect/RecordComponent", "typeAnnotations", "[B");

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("java/lang/Class", "getRecordComponents0", "()[Ljava/lang/reflect/RecordComponent;", record_components);
}

/// One parsed `record_component_info`.
struct Component {
    name: &'static str,
    desc: &'static str,
    signature: Option<&'static str>,
    annotations: Option<&'static [u8]>,
    type_annotations: Option<&'static [u8]>,
}

fn u2(b: &[u8], at: usize) -> usize {
    u16::from_be_bytes([b[at], b[at + 1]]) as usize
}

fn components(k: &'static Klass) -> Option<Vec<Component>> {
    let cf = k.cf.as_ref()?;
    let b = cf.attr_bytes(cf.attr("Record")?);
    let utf8 = |i: usize| cf.utf8(i as u16).ok().map(|s| s.as_str());
    let mut out = Vec::with_capacity(u2(b, 0));
    let mut p = 2;
    for _ in 0..u2(b, 0) {
        let mut c = Component { name: utf8(u2(b, p))?, desc: utf8(u2(b, p + 2))?, signature: None, annotations: None, type_annotations: None };
        let attrs = u2(b, p + 4);
        p += 6;
        for _ in 0..attrs {
            let len = u32::from_be_bytes(b[p + 2..p + 6].try_into().ok()?) as usize;
            let body = &b[p + 6..p + 6 + len];
            match utf8(u2(b, p))? {
                "Signature" => c.signature = utf8(u2(body, 0)),
                "RuntimeVisibleAnnotations" => c.annotations = Some(body),
                "RuntimeVisibleTypeAnnotations" => c.type_annotations = Some(body),
                _ => {}
            }
            p += 6 + len;
        }
        out.push(c);
    }
    Some(out)
}


fn record_components(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let Some(k) = klass(a[0]) else { return ret_ref(std::ptr::null_mut()) };
    let Some(list) = components(k) else { return ret_ref(std::ptr::null_mut()) };
    let rc_class = crate::loader::load(RC).expect("RecordComponent");
    rt::initialize(t, rc_class)?;
    let out = new_object_array(t, rc_class, list.len())?;
    for (i, c) in list.iter().enumerate() {
        let rc = rt::alloc_instance(t, rc_class)?;
        store(out, i, rc);
        let name = rt::intern_str(t, c.name)?;
        let ty = rt::mirror_for_desc(t, k, c.desc)?;
        let sig = optional_string(t, c.signature)?;
        let ann = byte_array(t, c.annotations)?;
        let type_ann = byte_array(t, c.type_annotations)?;
        let accessor_desc = format!("(){}", c.desc);
        let slot = k.methods().iter().position(|m| m.name == c.name && m.desc == accessor_desc.as_str() && !m.is_static());
        let accessor = match slot {
            Some(slot) => executable_object(t, a[0], k, slot, false)?,
            None => std::ptr::null_mut(),
        };
        // SAFETY: a RecordComponent instance and its declared reference fields.
        unsafe {
            obj::write::<Oop>(rc, rc_clazz(), a[0] as Oop);
            obj::write::<Oop>(rc, rc_name(), name);
            obj::write::<Oop>(rc, rc_type(), ty);
            obj::write::<Oop>(rc, rc_accessor(), accessor);
            obj::write::<Oop>(rc, rc_signature(), sig);
            obj::write::<Oop>(rc, rc_annotations(), ann);
            obj::write::<Oop>(rc, rc_type_annotations(), type_ann);
        }
    }
    ret_ref(out)
}
