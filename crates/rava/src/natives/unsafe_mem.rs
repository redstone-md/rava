//! `jdk.internal.misc.Unsafe`. An access address is `base + offset`, where a null base makes
//! `offset` absolute. Static fields are exposed with a null base and the absolute address of
//! their slot as the offset, so every static access goes through the same path.

use super::{ret_bool, ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{Oop, ARRAY_BASE_OFFSET};
use crate::klass::Klass;
use crate::runtime::{self as rt, throw_new};
use std::sync::atomic::{fence, AtomicI32, AtomicI64, Ordering};

const U: &str = "jdk/internal/misc/Unsafe";

#[inline(always)]
fn addr(base: u64, offset: u64) -> *mut u8 {
    (base as usize).wrapping_add(offset as usize) as *mut u8
}

macro_rules! accessors {
    ($r:ident, $get:expr, $put:expr, $desc_get:expr, $desc_put:expr, $ty:ty, $enc:expr, $dec:expr) => {
        $r.add(U, $get, $desc_get, |_, a| {
            // SAFETY: Unsafe's contract: the caller supplies a valid base/offset pair.
            let v: $ty = unsafe { std::ptr::read_volatile(addr(a[1], a[2]) as *const $ty) };
            Ok($enc(v))
        });
        $r.add(U, $put, $desc_put, |_, a| {
            let v: $ty = $dec(a[4]);
            // SAFETY: as above.
            unsafe { std::ptr::write_volatile(addr(a[1], a[2]) as *mut $ty, v) };
            if std::mem::size_of::<$ty>() == 8 {
                crate::gc::cards::dirty(addr(a[1], a[2]) as usize);
            }
            ret_void()
        });
    };
}

macro_rules! typed {
    ($r:ident, $name:literal, $d:literal, $ty:ty, $enc:expr, $dec:expr) => {
        accessors!($r, concat!("get", $name), concat!("put", $name), concat!("(Ljava/lang/Object;J)", $d),
            concat!("(Ljava/lang/Object;J", $d, ")V"), $ty, $enc, $dec);
        accessors!($r, concat!("get", $name, "Volatile"), concat!("put", $name, "Volatile"),
            concat!("(Ljava/lang/Object;J)", $d), concat!("(Ljava/lang/Object;J", $d, ")V"), $ty, $enc, $dec);
    };
}

/// Compare-and-exchange of a reference (`a`: this, base, offset, expected, new): the witness.
/// A null base names a static slot (full width); otherwise a heap field or element, compressed
/// when heap references are.
fn cas_reference(a: &[u64]) -> u64 {
    let at = addr(a[1], a[2]) as usize;
    crate::gc::cards::dirty(at);
    if a[1] != 0 && crate::heap::COMPRESSED {
        // SAFETY: an aligned reference field or element (Unsafe's contract).
        let cell = unsafe { &*(at as *const AtomicI32) };
        let (expected, new) = (crate::heap::encode(a[4] as Oop) as i32, crate::heap::encode(a[5] as Oop) as i32);
        let witness = cell.compare_exchange(expected, new, Ordering::SeqCst, Ordering::SeqCst).unwrap_or_else(|v| v);
        return crate::heap::decode(witness as u32 as u64) as u64;
    }
    // SAFETY: an aligned reference slot (Unsafe's contract).
    let cell = unsafe { &*(at as *const AtomicI64) };
    cell.compare_exchange(a[4] as i64, a[5] as i64, Ordering::SeqCst, Ordering::SeqCst).unwrap_or_else(|v| v) as u64
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(U, "registerNatives", "()V", |_, _| ret_void());
    typed!(r, "Int", "I", i32, |v: i32| v as i64 as u64, |s: u64| s as i32);
    typed!(r, "Long", "J", i64, |v: i64| v as u64, |s: u64| s as i64);
    for (get, put) in [("getReference", "putReference"), ("getReferenceVolatile", "putReferenceVolatile")] {
        r.add(U, get, "(Ljava/lang/Object;J)Ljava/lang/Object;", |_, a| {
            // SAFETY: Unsafe's contract: a reference field or element, or (null base) a static slot.
            Ok(unsafe { if a[1] == 0 { std::ptr::read_volatile(a[2] as *const u64) } else { crate::heap::obj::load_ref_at(addr(a[1], a[2]) as usize) as u64 } })
        });
        r.add(U, put, "(Ljava/lang/Object;JLjava/lang/Object;)V", |_, a| {
            // SAFETY: as above.
            unsafe {
                if a[1] == 0 {
                    std::ptr::write_volatile(a[2] as *mut u64, a[4]);
                    crate::gc::cards::dirty(a[2] as usize);
                } else {
                    crate::heap::obj::store_ref_at(addr(a[1], a[2]) as usize, a[4] as Oop);
                }
            }
            fence(Ordering::SeqCst);
            ret_void()
        });
    }
    typed!(r, "Boolean", "Z", u8, |v: u8| (v != 0) as u64, |s: u64| (s & 1) as u8);
    typed!(r, "Byte", "B", i8, |v: i8| v as i64 as u64, |s: u64| s as i8);
    typed!(r, "Short", "S", i16, |v: i16| v as i64 as u64, |s: u64| s as i16);
    typed!(r, "Char", "C", u16, |v: u16| v as u64, |s: u64| s as u16);
    typed!(r, "Float", "F", u32, |v: u32| v as u64, |s: u64| s as u32);
    typed!(r, "Double", "D", u64, |v: u64| v, |s: u64| s);
    r.add(U, "compareAndSetInt", "(Ljava/lang/Object;JII)Z", |_, a| {
        // SAFETY: aligned int address per Unsafe contract.
        let cell = unsafe { &*(addr(a[1], a[2]) as *const AtomicI32) };
        ret_bool(cell.compare_exchange(a[4] as i32, a[5] as i32, Ordering::SeqCst, Ordering::SeqCst).is_ok())
    });
    r.add(U, "compareAndExchangeInt", "(Ljava/lang/Object;JII)I", |_, a| {
        let cell = unsafe { &*(addr(a[1], a[2]) as *const AtomicI32) };
        ret_int(cell.compare_exchange(a[4] as i32, a[5] as i32, Ordering::SeqCst, Ordering::SeqCst).unwrap_or_else(|v| v))
    });
    r.add(U, "compareAndSetLong", "(Ljava/lang/Object;JJJ)Z", |_, a| {
        let cell = unsafe { &*(addr(a[1], a[2]) as *const AtomicI64) };
        ret_bool(cell.compare_exchange(a[4] as i64, a[6] as i64, Ordering::SeqCst, Ordering::SeqCst).is_ok())
    });
    r.add(U, "compareAndExchangeLong", "(Ljava/lang/Object;JJJ)J", |_, a| {
        let cell = unsafe { &*(addr(a[1], a[2]) as *const AtomicI64) };
        ret_long(cell.compare_exchange(a[4] as i64, a[6] as i64, Ordering::SeqCst, Ordering::SeqCst).unwrap_or_else(|v| v))
    });
    r.add(U, "compareAndSetReference", "(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Z", |_, a| {
        ret_bool(cas_reference(a) == a[4])
    });
    r.add(U, "compareAndExchangeReference", "(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;", |_, a| {
        Ok(cas_reference(a))
    });
    r.add(U, "arrayBaseOffset0", "(Ljava/lang/Class;)I", |_, _| ret_int(ARRAY_BASE_OFFSET as i32));
    r.add(U, "arrayIndexScale0", "(Ljava/lang/Class;)I", |_, a| {
        ret_int(rt::klass_of_mirror(a[1] as Oop).map_or(8, |k| k.elem_size()) as i32)
    });
    r.add(U, "addressSize0", "()I", |_, _| ret_int(8));
    r.add(U, "pageSize", "()I", |_, _| ret_int(4096));
    r.add(U, "objectFieldOffset1", "(Ljava/lang/Class;Ljava/lang/String;)J", |t, a| {
        let k = rt::klass_of_mirror(a[1] as Oop).ok_or_else(|| throw_new(t, "java/lang/InternalError", "primitive"))?;
        let name = rt::java_string_to_rust(a[2] as Oop);
        match k.supers().find_map(|c| c.fields.iter().find(|f| !f.is_static() && f.name == name.as_str())) {
            Some(f) => ret_long(f.offset as i64),
            None => Err(throw_new(t, "java/lang/InternalError", &name)),
        }
    });
    r.add(U, "objectFieldOffset0", "(Ljava/lang/reflect/Field;)J", |t, a| {
        let (k, i) = super::reflect::field_of(a[1] as Oop);
        let f = &k.fields[i];
        if f.is_static() {
            return Err(throw_new(t, "java/lang/IllegalArgumentException", "static field"));
        }
        ret_long(f.offset as i64)
    });
    r.add(U, "staticFieldOffset0", "(Ljava/lang/reflect/Field;)J", |_, a| {
        let (k, i) = super::reflect::field_of(a[1] as Oop);
        ret_long(static_slot_address(k, i) as i64)
    });
    r.add(U, "staticFieldBase0", "(Ljava/lang/reflect/Field;)Ljava/lang/Object;", |_, _| ret_ref(std::ptr::null_mut()));
    r.add(U, "shouldBeInitialized0", "(Ljava/lang/Class;)Z", |_, a| {
        ret_bool(rt::klass_of_mirror(a[1] as Oop).is_some_and(|k| k.init.state() != crate::klass::InitState::Initialized))
    });
    r.add(U, "ensureClassInitialized0", "(Ljava/lang/Class;)V", |t, a| {
        if let Some(k) = rt::klass_of_mirror(a[1] as Oop) {
            rt::initialize(t, k)?;
        }
        ret_void()
    });
    r.add(U, "allocateInstance", "(Ljava/lang/Class;)Ljava/lang/Object;", |t, a| {
        let k = rt::klass_of_mirror(a[1] as Oop).ok_or_else(|| throw_new(t, "java/lang/InstantiationException", "primitive"))?;
        rt::initialize(t, k)?;
        ret_ref(rt::alloc_instance(t, k)?)
    });
    r.add(U, "throwException", "(Ljava/lang/Throwable;)V", |_, a| Err(crate::thread::Throw(a[1] as Oop)));
    r.add(U, "fullFence", "()V", |_, _| { fence(Ordering::SeqCst); ret_void() });
    r.add(U, "loadFence", "()V", |_, _| { fence(Ordering::Acquire); ret_void() });
    r.add(U, "storeFence", "()V", |_, _| { fence(Ordering::Release); ret_void() });
    r.add(U, "allocateMemory0", "(J)J", |_, a| ret_long(unsafe { libc::malloc(a[1].max(1) as usize) } as i64));
    r.add(U, "reallocateMemory0", "(JJ)J", |_, a| ret_long(unsafe { libc::realloc(a[1] as *mut libc::c_void, a[3].max(1) as usize) } as i64));
    r.add(U, "freeMemory0", "(J)V", |_, a| { unsafe { libc::free(a[1] as *mut libc::c_void) }; ret_void() });
    r.add(U, "setMemory0", "(Ljava/lang/Object;JJB)V", |_, a| {
        unsafe { std::ptr::write_bytes(addr(a[1], a[2]), a[6] as u8, a[4] as usize) };
        ret_void()
    });
    r.add(U, "copyMemory0", "(Ljava/lang/Object;JLjava/lang/Object;JJ)V", |_, a| {
        unsafe { std::ptr::copy(addr(a[1], a[2]), addr(a[4], a[5]), a[7] as usize) };
        crate::gc::cards::dirty_range(addr(a[4], a[5]) as usize, a[7] as usize);
        ret_void()
    });
    r.add(U, "getLoadAverage0", "([DI)I", |_, _| ret_int(-1));
    r.add(U, "unpark", "(Ljava/lang/Object;)V", |_, a| {
        if let Some(rec) = super::thread::record_of(a[1] as Oop) {
            rec.parker.unpark();
        }
        ret_void()
    });
    r.add("java/util/concurrent/atomic/AtomicLong", "VMSupportsCS8", "()Z", |_, _| ret_bool(true));
    r.add(U, "park", "(ZJ)V", |t, a| {
        let (absolute, time) = (a[1] != 0, a[2] as i64);
        let Some(rec) = t.gc_record else { return ret_void() };
        let jt = t.java_thread;
        if super::thread::is_interrupted(jt, false) {
            return ret_void();
        }
        let deadline = match (absolute, time) {
            (false, 0) => None,
            (false, n) if n > 0 => Some(std::time::Instant::now() + std::time::Duration::from_nanos(n as u64)),
            (true, ms) => {
                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
                Some(std::time::Instant::now() + std::time::Duration::from_millis((ms - now_ms).max(0) as u64))
            }
            _ => return ret_void(),
        };
        crate::gc::safepoint::blocking(t, || rec.parker.park(deadline, &|| super::thread::is_interrupted(jt, false)));
        ret_void()
    });
}

/// Absolute address of static field `i` of `k` (the Unsafe "offset" for a null base).
pub(crate) fn static_slot_address(k: &'static Klass, i: usize) -> usize {
    &k.statics[k.fields[i].offset as usize] as *const _ as usize
}

