//! JNI references. A `jobject` is the object's address; what keeps the object alive (and, as a
//! root the collector does not move objects of, in place) is where the reference is recorded:
//! local references in the thread's `handles` (dropped when the native returns, or at
//! `PopLocalFrame`), global ones in a VM-wide table. Arguments of a native call are in its
//! caller's value-stack slots, which the collector scans conservatively and so also pins.

use super::Scope;
use crate::heap::Oop;
use crate::thread::Thread;
use std::collections::HashMap;
use std::sync::Mutex;

/// A local reference to `o` (null stays null).
pub(super) fn local(t: &mut Thread, o: Oop) -> Oop {
    if !o.is_null() {
        touch(t);
        t.handles.push(o);
    }
    o
}

/// Records where the current native call's references start, before its first one.
pub(super) fn touch(t: &mut Thread) {
    if t.jni.scope.touched == 0 {
        t.jni.scope = Scope { touched: 1, mark: t.handles.len(), frames: t.jni.frames.len() };
    }
}

/// Opens the scope of a native call, returning the enclosing call's.
pub(super) fn open(t: &mut Thread) -> Scope {
    std::mem::take(&mut t.jni.scope)
}

/// Drops the local references and frames of the call `open` began, and returns to `outer`.
pub(super) fn close(t: &mut Thread, outer: Scope) {
    release(t);
    t.jni.scope = outer;
}

/// Drops the current scope's local references and frames.
pub(super) fn release(t: &mut Thread) {
    let s = t.jni.scope;
    if s.touched != 0 {
        t.handles.truncate(s.mark);
        t.jni.frames.truncate(s.frames);
    }
}

/// The first local reference `DeleteLocalRef` may drop: within the current call's innermost
/// local frame (none when the call made no reference).
pub(super) fn floor(t: &Thread) -> usize {
    let s = t.jni.scope;
    if s.touched == 0 {
        return t.handles.len();
    }
    t.jni.frames.last().copied().unwrap_or(0).max(s.mark)
}

/// Global references, counted per object (a reference made twice is deleted twice).
fn globals() -> &'static Mutex<HashMap<usize, usize>> {
    static G: std::sync::OnceLock<Mutex<HashMap<usize, usize>>> = std::sync::OnceLock::new();
    G.get_or_init(Default::default)
}

pub(super) fn new_global(o: Oop) -> Oop {
    if !o.is_null() {
        *globals().lock().unwrap().entry(o as usize).or_default() += 1;
    }
    o
}

pub(super) fn delete_global(o: Oop) {
    let mut g = globals().lock().unwrap();
    if let Some(n) = g.get_mut(&(o as usize)) {
        *n -= 1;
        if *n == 0 {
            g.remove(&(o as usize));
        }
    }
}

/// Global references, for the collector (precise roots it leaves in place).
pub(crate) fn roots(f: &mut dyn FnMut(Oop)) {
    for &o in globals().lock().unwrap().keys() {
        f(o as Oop);
    }
}

/// The exception a native raised (`Throw`, or a Java call that threw), taking it.
pub(super) fn take_exception(t: &mut Thread) -> Option<Oop> {
    let e = std::mem::replace(&mut t.jni.exception, std::ptr::null_mut());
    (!e.is_null()).then_some(e)
}
