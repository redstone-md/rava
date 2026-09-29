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
    r.add(
        "java/lang/StackTraceElement",
        "initStackTraceElement",
        "(Ljava/lang/StackTraceElement;Ljava/lang/StackFrameInfo;)V",
        init_stack_trace_element,
    );
}

fn fill_in_stack_trace(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let ex = a[0] as Oop;
    // SAFETY: live receiver.
    let ek = unsafe { obj::klass(ex) };
    // Skip this native, the Java fillInStackTrace wrappers, and the exception's constructors.
    let frames: Vec<(&'static Method, i32)> = t
        .frames()
        .skip_while(|(m, _)| m.name.as_str() == "fillInStackTrace")
        .skip_while(|(m, _)| m.name.as_str() == "<init>" && ek.is_subtype_of(m.holder))
        .filter(|(m, _)| !m.is_hidden_frame())
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
    // SAFETY: live Throwable.
    frames_of_backtrace(unsafe { obj::read::<Oop>(ex, backtrace_offset()) })
}

/// Decodes a backtrace `long[]` written by `fill_in_stack_trace`.
fn frames_of_backtrace(bt: Oop) -> Vec<(&'static Method, i32)> {
    if bt.is_null() {
        return Vec::new();
    }
    // SAFETY: a long[] of (method address, bci) pairs.
    unsafe { obj::array_slice::<i64>(bt).chunks_exact(2).map(|p| (&*(p[0] as *const Method), p[1] as i32)).collect() }
}

/// `initStackTraceElements(elements, backtrace, depth)`.
fn init_stack_trace_elements(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (elements, backtrace) = (a[0] as Oop, a[1] as Oop);
    for (i, (m, bci)) in frames_of_backtrace(backtrace).into_iter().enumerate() {
        // SAFETY: the element array has `depth` entries, pre-populated by the JDK.
        let e = unsafe { *obj::array_data::<Oop>(elements).add(i) };
        if !e.is_null() {
            fill_element(t, e, m, bci)?;
        }
    }
    ret_void()
}

fn fill_element(t: &mut Thread, e: Oop, m: &'static Method, bci: i32) -> JResult<()> {
    let class = rt::new_string(t, &m.holder.java_name())?;
    // SAFETY: live StackTraceElement; each string is stored as soon as it exists.
    unsafe { obj::write::<Oop>(e, ste_class(), class) };
    let method = rt::new_string(t, m.name.as_str())?;
    unsafe { obj::write::<Oop>(e, ste_method(), method) };
    if let Some(f) = source_file(m) {
        let file = rt::new_string(t, f)?;
        unsafe { obj::write::<Oop>(e, ste_file(), file) };
    }
    unsafe { obj::write::<i32>(e, ste_line(), if m.is_native() { -2 } else { line_number(m, bci) }) };
    Ok(())
}

vm_field!(sfi_member, "java/lang/StackFrameInfo", "memberName", "Ljava/lang/Object;");
vm_field!(sfi_bci, "java/lang/StackFrameInfo", "bci", "I");

/// `initStackTraceElement(element, stackFrameInfo)`.
fn init_stack_trace_element(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (e, sfi) = (a[0] as Oop, a[1] as Oop);
    // SAFETY: a StackFrameInfo filled by the stack walker.
    let (mn, bci) = unsafe { (obj::read::<Oop>(sfi, sfi_member()), obj::read::<i32>(sfi, sfi_bci())) };
    if let Some(m) = super::mh::member_target(mn) {
        fill_element(t, e, m, bci)?;
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
            out += &format!("\tat {}.{}({}:{})\n", m.holder.java_name(), m.name, file, line_number(m, bci));
        }
        if cause == cur {
            break;
        }
        cur = cause;
    }
    out
}
