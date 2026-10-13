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
    r.add("java/lang/System", "registerNatives", "()V", |_, _| ret_void());
    r.add("java/lang/System", "identityHashCode", "(Ljava/lang/Object;)I", |_, a| {
        let o = a[0] as Oop;
        ret_int(if o.is_null() { 0 } else { identity_hash(o) })
    });
    r.add("java/lang/System", "arraycopy", "(Ljava/lang/Object;ILjava/lang/Object;II)V", super::arraycopy::arraycopy);
    r.add("java/lang/System", "nanoTime", "()J", |_, _| ret_long(nanos_since_epoch() as i64));
    r.add("java/lang/System", "currentTimeMillis", "()J", |_, _| ret_long((nanos_since_epoch() / 1_000_000) as i64));
    r.add("java/lang/Class", "registerNatives", "()V", |_, _| ret_void());
    r.add("java/lang/Class", "desiredAssertionStatus0", "(Ljava/lang/Class;)Z", |_, a| {
        let k = rt::klass_of_mirror(a[0] as Oop).expect("class mirror");
        ret_bool(crate::vm::vm().options.assertions.enabled(&k.java_name(), k.loader().is_null()))
    });
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
    r.add("java/lang/Class", "getModifiers", "()I", |_, a| ret_int(class_modifiers(rt::klass_of_mirror(a[0] as Oop))));
    r.add("java/lang/Class", "initClassName", "()Ljava/lang/String;", class_init_name);
    r.add("java/lang/Float", "floatToRawIntBits", "(F)I", |_, a| ret_int(a[0] as u32 as i32));
    r.add("java/lang/Float", "intBitsToFloat", "(I)F", |_, a| Ok(a[0] as u32 as u64));
    r.add("java/lang/Double", "doubleToRawLongBits", "(D)J", |_, a| Ok(a[0]));
    r.add("java/lang/Double", "longBitsToDouble", "(J)D", |_, a| Ok(a[0]));
    r.add("java/lang/StringUTF16", "isBigEndian", "()Z", |_, _| ret_bool(cfg!(target_endian = "big")));
    r.add("java/lang/String", "intern", "()Ljava/lang/String;", |t, a| ret_ref(rt::intern(t, a[0] as Oop)?));
    r.add("java/lang/Runtime", "availableProcessors", "()I", |_, _| {
        ret_int(crate::processors() as i32)
    });
    // `System.console()`: which standard streams are terminals (bits: in, out, err).
    r.add("java/io/Console", "ttyStatus", "()I", |_, _| {
        // SAFETY: isatty only inspects the descriptors.
        let tty = |fd| unsafe { libc::isatty(fd) == 1 } as i32;
        ret_int(tty(0) | tty(1) << 1 | tty(2) << 2)
    });
    r.add("java/io/Console", "encoding", "()Ljava/lang/String;", |_, _| super::ret_ref(std::ptr::null_mut()));
    r.add("jdk/internal/misc/CDS", "isDumpingClassList0", "()Z", |_, _| ret_bool(false));
    r.add("jdk/internal/misc/CDS", "isDumpingArchive0", "()Z", |_, _| ret_bool(false));
    r.add("jdk/internal/misc/CDS", "isSharingEnabled0", "()Z", |_, _| ret_bool(false));
    r.add("jdk/internal/misc/CDS", "initializeFromArchive", "(Ljava/lang/Class;)V", |_, _| ret_void());
    r.add("jdk/internal/misc/CDS", "getRandomSeedForDumping", "()J", |_, _| ret_long(0));
    // JDK 25: no archive is configured (no CDS flags set).
    r.add("jdk/internal/misc/CDS", "getCDSConfigStatus", "()I", |_, _| ret_int(0));
    super::throwable::register(r);
}

/// `Class.getModifiers` (HotSpot `JVM_GetClassModifiers`): the `InnerClasses` flags for a
/// member class, else the written class flags; arrays take their element's visibility.
pub(crate) fn class_modifiers(k: Option<&'static crate::klass::Klass>) -> i32 {
    const PUBLIC_ABSTRACT_FINAL: i32 = 0x0411;
    let Some(k) = k else { return PUBLIC_ABSTRACT_FINAL };
    match k.kind {
        KlassKind::TypeArray { .. } => PUBLIC_ABSTRACT_FINAL,
        KlassKind::ObjArray { elem } => (class_modifiers(Some(elem)) & 0x0007) | 0x0410,
        KlassKind::Instance => {
            let cf = k.cf.as_ref().unwrap();
            let inner = cf.attr("InnerClasses").and_then(|a| {
                let b = cf.attr_bytes(a);
                let n = u16::from_be_bytes([b[0], b[1]]) as usize;
                (0..n).map(|i| &b[2 + 8 * i..10 + 8 * i]).find_map(|e| {
                    (cf.class_name(u16::from_be_bytes([e[0], e[1]])) == Some(cf.name)).then(|| u16::from_be_bytes([e[6], e[7]]))
                })
            });
            (inner.unwrap_or(k.access & 0x7631) & !0x0020) as i32
        }
    }
}

pub(crate) const HASH_SHIFT: u32 = 8;
pub(crate) const HASH_MASK: u64 = 0x7FFF_FFFF;

/// Identity hash stored in the mark word on first request (xor-shift, never 0).
pub(crate) fn identity_hash(o: Oop) -> i32 {
    // SAFETY: callers pass live non-null objects.
    let mark = unsafe { obj::mark(o) };
    let current = (mark.load(Ordering::Relaxed) >> HASH_SHIFT) & HASH_MASK;
    if current != 0 {
        return current as i32;
    }
    // `RAVA_HASH_SEED` fixes the sequence (reproducible measurements); else it differs per process.
    thread_local!(static SEED: std::cell::Cell<u32> = std::cell::Cell::new(
        0x9E37_79B9 ^ std::env::var("RAVA_HASH_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or_else(std::process::id)
    ));
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
    // A lock transition may change the other bits meanwhile: retry until the hash is in, ours
    // or another thread's (returning the field of a failed exchange could return 0, and the next
    // call a different hash).
    let mut old = mark.load(Ordering::Relaxed);
    loop {
        let present = (old >> HASH_SHIFT) & HASH_MASK;
        if present != 0 {
            return present as i32;
        }
        match mark.compare_exchange_weak(old, old | (h << HASH_SHIFT), Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return h as i32,
            Err(now) => old = now,
        }
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
    let dst = vm().heap.allocate(t, size).ok_or_else(|| throw_new(t, "java/lang/OutOfMemoryError", "Java heap space"))? as Oop;
    // SAFETY: both ranges are `size` bytes; the fresh copy gets a clean mark word.
    unsafe {
        std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, size);
        obj::mark(dst).store(0, Ordering::Relaxed);
    }
    ret_ref(dst)
}

fn nanos_since_epoch() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// `Class.initClassName`: the binary name, cached in the `name` field.
fn class_init_name(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let mirror = a[0] as Oop;
    let name = match rt::klass_of_mirror(mirror) {
        Some(k) => k.java_name(),
        None => primitive_name(rt::primitive_type_of_mirror(mirror)),
    };
    let s = rt::intern_str(t, &name)?;
    static NAME: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let off = rt::field_offset(&NAME, "java/lang/Class", "name", "Ljava/lang/String;");
    // SAFETY: live Class mirror.
    unsafe { obj::write_ref(mirror, off, s) };
    ret_ref(s)
}

pub(super) fn primitive_name(ty: Option<crate::types::BasicType>) -> String {
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
