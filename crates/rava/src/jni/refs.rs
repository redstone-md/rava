//! JNI references. A `jobject` is the object's address; what keeps the object alive (and, as a
//! root the collector does not move objects of, in place) is where the reference is recorded:
//! local references in the thread's `handles` (dropped when the native returns, or at
//! `PopLocalFrame`), global ones in a VM-wide table. Arguments of a native call are in its
//! caller's value-stack slots, which the collector scans conservatively and so also pins.

use crate::heap::Oop;
use crate::thread::Thread;
use std::collections::HashMap;
use std::sync::Mutex;

/// A local reference to `o` (null stays null).
pub(super) fn local(t: &mut Thread, o: Oop) -> Oop {
    if !o.is_null() {
        t.handles.push(o);
    }
    o
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
