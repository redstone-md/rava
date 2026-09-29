//! Stack traces: `Throwable.fillInStackTrace` records (method, bci) pairs in a `long[]`
//! backtrace; `StackTraceElement.initStackTraceElements` expands them lazily.

use super::{ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::method::Method;
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use crate::vm::vm;

vm_field!(backtrace_offset, "java/lang/Throwable", "backtrace", "Ljava/lang/Object;");
vm_field!(depth_offset, "java/lang/Throwable", "depth", "I");
vm_field!(message_offset, "java/lang/Throwable", "detailMessage", "Ljava/lang/String;");
vm_field!(cause_offset, "java/lang/Throwable", "cause", "Ljava/lang/Throwable;");
vm_field!(ste_class, "java/lang/StackTraceElement", "declaringClass", "Ljava/lang/String;");
vm_field!(ste_method, "java/lang/StackTraceElement", "methodName", "Ljava/lang/String;");
vm_field!(ste_file, "java/lang/StackTraceElement", "fileName", "Ljava/lang/String;");
vm_field!(ste_line, "java/lang/StackTraceElement", "lineNumber", "I");

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("java/lang/Throwable", "fillInStackTrace", "(I)Ljava/lang/Throwable;", fill_in_stack_trace);
    r.add(
        "java/lang/StackTraceElement",
        "initStackTraceElements",
        "([Ljava/lang/StackTraceElement;Ljava/lang/Object;I)V",
        init_stack_trace_elements,
    );
}

fn fill_in_stack_trace(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let ex = a[0] as Oop;
    // SAFETY: live receiver.
    let ek = unsafe { obj::klass(ex) };
    // Skip this native, the Java fillInStackTrace wrappers, and the exception's constructors.
    let frames: Vec<(&'static Method, i32)> = (0..t.frame_count())
        .filter_map(|n| t.caller_frame(n))
        .skip_while(|(m, _)| m.name.as_str() == "fillInStackTrace")
        .skip_while(|(m, _)| m.name.as_str() == "<init>" && ek.is_subtype_of(m.holder))
        .collect();
    let longs = vm().primitive_array_class(BasicType::Long).expect("[J");
    let bt = rt::alloc_array(t, longs, frames.len() as i32 * 2)?;
    // SAFETY: fresh long[] of twice the frame count.
    unsafe {
        let data = obj::array_slice::<i64>(bt);
        for (i, (m, bci)) in frames.iter().enumerate() {
            data[2 * i] = *m as *const Method as i64;
            data[2 * i + 1] = *bci as i64;
        }
        obj::write::<Oop>(ex, backtrace_offset(), bt);
        obj::write::<i32>(ex, depth_offset(), frames.len() as i32);
    }
    ret_ref(ex)
}

/// The recorded frames of an exception.
pub fn backtrace_of(ex: Oop) -> Vec<(&'static Method, i32)> {
    // SAFETY: live Throwable; backtrace is null or the long[] written above.
    unsafe {
        let bt = obj::read::<Oop>(ex, backtrace_offset());
        if bt.is_null() {
            return Vec::new();
        }
        obj::array_slice::<i64>(bt).chunks_exact(2).map(|p| (&*(p[0] as *const Method), p[1] as i32)).collect()
    }
}

fn init_stack_trace_elements(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (elements, backtrace_holder) = (a[0] as Oop, a[1] as Oop);
    let frames = backtrace_of(backtrace_holder);
    for (i, (m, bci)) in frames.iter().enumerate() {
        // SAFETY: the element array has `depth` entries, pre-populated by the JDK.
        let e = unsafe { *obj::array_data::<Oop>(elements).add(i) };
        if e.is_null() {
            continue;
        }
        let class = rt::new_string(t, &m.holder.name.as_str().replace('/', "."))?;
        let method = rt::new_string(t, m.name.as_str())?;
        let file = match source_file(m) {
            Some(f) => rt::new_string(t, f)?,
            None => std::ptr::null_mut(),
        };
        // SAFETY: live StackTraceElement.
        unsafe {
            obj::write::<Oop>(e, ste_class(), class);
            obj::write::<Oop>(e, ste_method(), method);
            obj::write::<Oop>(e, ste_file(), file);
            obj::write::<i32>(e, ste_line(), if m.is_native() { -2 } else { line_number(m, *bci) });
        }
    }
    ret_void()
}

fn source_file(m: &Method) -> Option<&'static str> {
    let cf = m.holder.cf.as_ref()?;
    let a = cf.attr("SourceFile")?;
    let b = cf.attr_bytes(a);
    cf.utf8(u16::from_be_bytes([b[0], b[1]])).ok().map(|s| s.as_str())
}

pub fn line_number(m: &Method, bci: i32) -> i32 {
    let (Some(code), Some(cf)) = (m.code, m.holder.cf.as_ref()) else { return -1 };
    let Some(table) = code.line_numbers else { return -1 };
    let b = cf.attr_bytes(table);
    let mut line = -1;
    for e in b[2..].chunks_exact(4) {
        let (start, l) = (u16::from_be_bytes([e[0], e[1]]) as i32, u16::from_be_bytes([e[2], e[3]]) as i32);
        if start <= bci {
            line = l;
        }
    }
    line
}

/// `java.lang.Foo: message` plus `at` lines and causes, for reporting uncaught exceptions.
pub fn describe(ex: Oop) -> String {
    let mut out = String::new();
    let mut cur = ex;
    for depth in 0..8 {
        if cur.is_null() {
            break;
        }
        // SAFETY: live Throwable.
        let (msg, cause) = unsafe { (obj::read::<Oop>(cur, message_offset()), obj::read::<Oop>(cur, cause_offset())) };
        let prefix = if depth == 0 { "" } else { "Caused by: " };
        out += &format!("{prefix}{}", rt::class_name_of(cur));
        if !msg.is_null() {
            out += &format!(": {}", rt::java_string_to_rust(msg));
        }
        out.push('\n');
        for (m, bci) in backtrace_of(cur) {
            let file = source_file(m).unwrap_or("Unknown Source");
            out += &format!("\tat {}.{}({}:{})\n", m.holder.name.as_str().replace('/', "."), m.name, file, line_number(m, bci));
        }
        if cause == cur {
            break;
        }
        cur = cause;
    }
    out
}
