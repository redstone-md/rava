//! `java.lang` VM natives: identity, cloning, array copy, time, float bits, class queries.

use super::{ret_bool, ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop, ARRAY_BASE_OFFSET};
use crate::klass::{KlassKind, ACC_INTERFACE};
use crate::runtime::{self as rt, null_pointer, throw_new};
use crate::thread::{JResult, Thread};
use crate::vm::vm;
use std::sync::atomic::Ordering;

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("java/lang/Object", "hashCode", "()I", |_, a| ret_int(identity_hash(a[0] as Oop)));
    r.add("java/lang/Object", "getClass", "()Ljava/lang/Class;", |t, a| {
        let k = unsafe { obj::klass(a[0] as Oop) };
        ret_ref(rt::mirror_of(t, k)?)
    });
    r.add("java/lang/Object", "clone", "()Ljava/lang/Object;", object_clone);
    r.add("java/lang/Object", "notify", "()V", |_, _| ret_void());
    r.add("java/lang/Object", "notifyAll", "()V", |_, _| ret_void());
    r.add("java/lang/System", "registerNatives", "()V", |_, _| ret_void());
    r.add("java/lang/System", "identityHashCode", "(Ljava/lang/Object;)I", |_, a| {
        let o = a[0] as Oop;
        ret_int(if o.is_null() { 0 } else { identity_hash(o) })
    });
    r.add("java/lang/System", "arraycopy", "(Ljava/lang/Object;ILjava/lang/Object;II)V", arraycopy);
    r.add("java/lang/System", "nanoTime", "()J", |_, _| ret_long(nanos_since_epoch() as i64));
    r.add("java/lang/System", "currentTimeMillis", "()J", |_, _| ret_long((nanos_since_epoch() / 1_000_000) as i64));
    r.add("java/lang/Class", "registerNatives", "()V", |_, _| ret_void());
    r.add("java/lang/Class", "desiredAssertionStatus0", "(Ljava/lang/Class;)Z", |_, _| ret_bool(false));
    r.add("java/lang/Class", "getPrimitiveClass", "(Ljava/lang/String;)Ljava/lang/Class;", |t, a| {
        let name = rt::java_string_to_rust(a[0] as Oop);
        let ty = rt::primitive_by_name(&name).ok_or_else(|| throw_new(t, "java/lang/IllegalArgumentException", &name))?;
        ret_ref(rt::primitive_mirror(t, ty)?)
    });
    r.add("java/lang/Class", "isArray", "()Z", |_, a| ret_bool(rt::klass_of_mirror(a[0] as Oop).is_some_and(|k| k.is_array())));
    r.add("java/lang/Class", "isPrimitive", "()Z", |_, a| ret_bool(rt::klass_of_mirror(a[0] as Oop).is_none()));
    r.add("java/lang/Class", "isInterface", "()Z", |_, a| {
        ret_bool(rt::klass_of_mirror(a[0] as Oop).is_some_and(|k| k.access & ACC_INTERFACE != 0))
    });
    r.add("java/lang/Class", "isInstance", "(Ljava/lang/Object;)Z", |_, a| {
        let o = a[1] as Oop;
        let target = rt::klass_of_mirror(a[0] as Oop);
        ret_bool(!o.is_null() && target.is_some_and(|k| unsafe { obj::klass(o) }.is_subtype_of(k)))
    });
    r.add("java/lang/Class", "isAssignableFrom", "(Ljava/lang/Class;)Z", |t, a| {
        if a[1] == 0 {
            return Err(null_pointer(t));
        }
        let (to, from) = (rt::klass_of_mirror(a[0] as Oop), rt::klass_of_mirror(a[1] as Oop));
        ret_bool(match (to, from) {
            (Some(to), Some(from)) => from.is_subtype_of(to),
            _ => a[0] == a[1],
        })
    });
    r.add("java/lang/Class", "getSuperclass", "()Ljava/lang/Class;", |t, a| {
        match rt::klass_of_mirror(a[0] as Oop).filter(|k| !k.is_interface()).and_then(|k| k.super_klass) {
            Some(s) => ret_ref(rt::mirror_of(t, s)?),
            None => ret_ref(std::ptr::null_mut()),
        }
    });
    r.add("java/lang/Class", "getModifiers", "()I", |_, a| {
        ret_int(rt::klass_of_mirror(a[0] as Oop).map_or(0x0411, |k| k.access as i32 & 0x0e1f))
    });
    r.add("java/lang/Class", "initClassName", "()Ljava/lang/String;", class_init_name);
    r.add("java/lang/Float", "floatToRawIntBits", "(F)I", |_, a| ret_int(a[0] as u32 as i32));
    r.add("java/lang/Float", "intBitsToFloat", "(I)F", |_, a| Ok(a[0] as u32 as u64));
    r.add("java/lang/Double", "doubleToRawLongBits", "(D)J", |_, a| Ok(a[0]));
    r.add("java/lang/Double", "longBitsToDouble", "(J)D", |_, a| Ok(a[0]));
    r.add("java/lang/StringUTF16", "isBigEndian", "()Z", |_, _| ret_bool(cfg!(target_endian = "big")));
    r.add("java/lang/String", "intern", "()Ljava/lang/String;", |t, a| ret_ref(rt::intern(t, a[0] as Oop)?));
    r.add("java/lang/Runtime", "availableProcessors", "()I", |_, _| {
        ret_int(std::thread::available_parallelism().map_or(1, |n| n.get()) as i32)
    });
    r.add("jdk/internal/misc/CDS", "isDumpingClassList0", "()Z", |_, _| ret_bool(false));
    r.add("jdk/internal/misc/CDS", "isDumpingArchive0", "()Z", |_, _| ret_bool(false));
    r.add("jdk/internal/misc/CDS", "isSharingEnabled0", "()Z", |_, _| ret_bool(false));
    r.add("jdk/internal/misc/CDS", "initializeFromArchive", "(Ljava/lang/Class;)V", |_, _| ret_void());
    r.add("jdk/internal/misc/CDS", "getRandomSeedForDumping", "()J", |_, _| ret_long(0));
    super::throwable::register(r);
}

const HASH_SHIFT: u32 = 8;
const HASH_MASK: u64 = 0x7FFF_FFFF;

/// Identity hash stored in the mark word on first request (xor-shift, never 0).
pub(crate) fn identity_hash(o: Oop) -> i32 {
    // SAFETY: callers pass live non-null objects.
    let mark = unsafe { obj::mark(o) };
    let current = (mark.load(Ordering::Relaxed) >> HASH_SHIFT) & HASH_MASK;
    if current != 0 {
        return current as i32;
    }
    thread_local!(static SEED: std::cell::Cell<u32> = std::cell::Cell::new(0x9E37_79B9 ^ std::process::id()));
    let h = SEED.with(|s| {
        let mut x = s.get();
        loop {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if x as u64 & HASH_MASK != 0 {
                s.set(x);
                return x as u64 & HASH_MASK;
            }
        }
    });
    let old = mark.load(Ordering::Relaxed);
    let new = old | (h << HASH_SHIFT);
    match mark.compare_exchange(old, new, Ordering::AcqRel, Ordering::Relaxed) {
        Ok(_) => h as i32,
        Err(now) => ((now >> HASH_SHIFT) & HASH_MASK) as i32,
    }
}

fn object_clone(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let src = a[0] as Oop;
    // SAFETY: live receiver.
    let k = unsafe { obj::klass(src) };
    let size = if k.is_array() {
        ARRAY_BASE_OFFSET as usize + k.elem_size() as usize * unsafe { obj::array_length(src) } as usize
    } else {
        let cloneable = vm().wk.cloneable.get().copied().expect("Cloneable");
        if !k.is_subtype_of(cloneable) {
            return Err(throw_new(t, "java/lang/CloneNotSupportedException", &k.name.as_str().replace('/', ".")));
        }
        k.instance_size() as usize
    };
    let dst = vm().heap.allocate(size).ok_or_else(|| throw_new(t, "java/lang/OutOfMemoryError", "Java heap space"))? as Oop;
    // SAFETY: both ranges are `size` bytes; the fresh copy gets a clean mark word.
    unsafe {
        std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, size);
        obj::mark(dst).store(0, Ordering::Relaxed);
    }
    ret_ref(dst)
}

fn arraycopy(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (src, sp, dst, dp, len) = (a[0] as Oop, a[1] as i32, a[2] as Oop, a[3] as i32, a[4] as i32);
    if src.is_null() || dst.is_null() {
        return Err(null_pointer(t));
    }
    // SAFETY: both are live objects.
    let (sk, dk) = unsafe { (obj::klass(src), obj::klass(dst)) };
    let compatible = match (&sk.kind, &dk.kind) {
        (KlassKind::TypeArray { elem: x }, KlassKind::TypeArray { elem: y }) => x == y,
        (KlassKind::ObjArray { .. }, KlassKind::ObjArray { .. }) => true,
        _ => false,
    };
    if !compatible {
        let msg = format!("arraycopy: type mismatch: can not copy {} into {}", sk.name, dk.name);
        return Err(throw_new(t, "java/lang/ArrayStoreException", &msg));
    }
    // SAFETY: both are arrays.
    let (sl, dl) = unsafe { (obj::array_length(src), obj::array_length(dst)) };
    if len < 0 || sp < 0 || dp < 0 || sp as i64 + len as i64 > sl as i64 || dp as i64 + len as i64 > dl as i64 {
        let msg = format!("arraycopy: last source index {} out of bounds for length {sl}", sp as i64 + len as i64);
        return Err(throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", &msg));
    }
    if let (KlassKind::ObjArray { .. }, KlassKind::ObjArray { elem: de }) = (&sk.kind, &dk.kind) {
        if !sk.is_subtype_of(dk) {
            // Element-wise store check; copies the prefix that fits, then throws.
            for i in 0..len as usize {
                // SAFETY: indices within the checked bounds.
                let v = unsafe { *obj::array_data::<Oop>(src).add(sp as usize + i) };
                if !v.is_null() && !unsafe { obj::klass(v) }.is_subtype_of(de) {
                    return Err(throw_new(t, "java/lang/ArrayStoreException", &rt::class_name_of(v)));
                }
                unsafe { *obj::array_data::<Oop>(dst).add(dp as usize + i) = v };
            }
            return ret_void();
        }
    }
    let es = sk.elem_size() as usize;
    // SAFETY: checked bounds; `copy` handles overlap (same array).
    unsafe {
        std::ptr::copy(
            obj::array_data::<u8>(src).add(sp as usize * es),
            obj::array_data::<u8>(dst).add(dp as usize * es),
            len as usize * es,
        )
    };
    ret_void()
}

fn nanos_since_epoch() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// `Class.initClassName`: the binary name, cached in the `name` field.
fn class_init_name(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let mirror = a[0] as Oop;
    let name = match rt::klass_of_mirror(mirror) {
        Some(k) if k.is_array() => k.name.as_str().replace('/', "."),
        Some(k) => k.name.as_str().replace('/', "."),
        None => primitive_name(rt::primitive_type_of_mirror(mirror)),
    };
    let s = rt::new_string(t, &name)?;
    let s = rt::intern(t, s)?;
    static NAME: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let off = rt::field_offset(&NAME, "java/lang/Class", "name", "Ljava/lang/String;");
    // SAFETY: live Class mirror.
    unsafe { obj::write::<Oop>(mirror, off, s) };
    ret_ref(s)
}

fn primitive_name(ty: Option<crate::types::BasicType>) -> String {
    use crate::types::BasicType::*;
    match ty {
        Some(Boolean) => "boolean",
        Some(Char) => "char",
        Some(Float) => "float",
        Some(Double) => "double",
        Some(Byte) => "byte",
        Some(Short) => "short",
        Some(Int) => "int",
        Some(Long) => "long",
        _ => "void",
    }
    .into()
}
