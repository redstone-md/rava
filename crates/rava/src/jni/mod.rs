//! The Java Native Interface: shared libraries loaded through `System.load`/`loadLibrary` and
//! the native methods they implement (`docs/gui-roadmap.md`). The JDK's own natives stay in
//! `natives/`, written in Rust; a native method the VM does not supply binds here.
//!
//! - `libs`: `dlopen`, `JNI_OnLoad`, symbol lookup by JNI name, `RegisterNatives`.
//! - `call`: the call itself, a System V call from the interpreter's argument slots, made in
//!   the native state (collections proceed meanwhile).
//! - `refs`: local and global references (`jobject` is the object's address; references are
//!   roots the collector leaves in place).
//! - `env*`: the `JNIEnv` function table; each function switches the thread back to running
//!   Java for its duration (`enter`).
//! - `vm_iface`: the `JavaVM` invocation interface.

mod call;
mod env;
mod env_call;
mod env_data;
mod env_field;
mod libs;
mod refs;
mod stub;
mod vm_iface;

pub(crate) use call::invoke;
pub(crate) use libs::{entry, load_for_java, symbol};
pub(crate) use refs::roots;

use crate::gc::safepoint::{self, Record, NATIVE};
use crate::thread::Thread;
use std::sync::atomic::Ordering::SeqCst;

/// The JNI version this VM implements (21's `JNI_VERSION_21`).
pub(crate) const VERSION: i32 = 0x0015_0000;

/// A thread's `JNIEnv`: what native code receives is the address of `functions`.
#[repr(C)]
pub(crate) struct Env {
    functions: *const usize,
}

/// A thread's JNI state, inside its `Thread` (an environment pointer finds its thread).
pub(crate) struct JniThread {
    env: Env,
    /// Starts of the local frames `PushLocalFrame` opened, as lengths of `Thread::handles`.
    pub(crate) frames: Vec<usize>,
    /// The exception native code has pending (`Throw`, or a Java call that threw); a root.
    pub(crate) exception: crate::heap::Oop,
    /// The local references of the innermost native call (`refs::open`); generated call stubs
    /// save, clear and test it at fixed offsets.
    pub(crate) scope: Scope,
}

/// Where the innermost native call's local references start, recorded when it makes its first
/// one (`refs::touch`): a call that makes none has nothing to release.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub(crate) struct Scope {
    /// Non-zero once the call made a local reference or frame.
    touched: u64,
    /// `Thread::handles` and `JniThread::frames` lengths at that point.
    mark: usize,
    frames: usize,
}

impl JniThread {
    pub(crate) fn new() -> JniThread {
        JniThread { env: Env { functions: env::table() }, frames: Vec::new(), exception: std::ptr::null_mut(), scope: Scope::default() }
    }
}

/// `t`'s `JNIEnv*`.
pub(crate) fn env_of(t: &mut Thread) -> *mut Env {
    &mut t.jni.env
}

/// The thread whose `JNIEnv*` is `env`.
fn thread_of(env: *mut Env) -> &'static mut Thread {
    let at = std::mem::offset_of!(Thread, jni) + std::mem::offset_of!(JniThread, env);
    // SAFETY: every `Env` handed out lives inside a `Thread` at that offset, which outlives
    // native code running on it.
    unsafe { &mut *((env as usize - at) as *mut Thread) }
}

thread_local!(static CURRENT: std::cell::Cell<*mut Thread> = const { std::cell::Cell::new(std::ptr::null_mut()) });

/// Records `t` as the thread running on this OS thread (`JavaVM::GetEnv`).
pub(crate) fn set_current(t: *mut Thread) {
    CURRENT.with(|c| c.set(t));
}

pub(crate) fn current() -> *mut Thread {
    CURRENT.with(|c| c.get())
}

/// While alive, the thread runs Java (a JNI function called from native code); dropping it
/// returns the thread to the native state it was in.
pub(super) struct InVm(Option<&'static Record>);

impl Drop for InVm {
    fn drop(&mut self) {
        if let Some(rec) = self.0 {
            safepoint::enter_native(rec);
        }
    }
}

/// Enters the VM from a JNI function: the calling thread, running Java until the guard drops.
pub(super) fn enter(env: *mut Env) -> (&'static mut Thread, InVm) {
    let t = thread_of(env);
    let rec = t.gc_record.filter(|r| r.state.load(SeqCst) == NATIVE);
    if let Some(r) = rec {
        safepoint::leave_native(r);
    }
    (t, InVm(rec))
}

/// A C `va_list` (System V: offsets into the register save area, then the stack).
#[cfg(not(windows))]
#[repr(C)]
pub(super) struct VaList {
    gp_offset: u32,
    fp_offset: u32,
    overflow: *mut u64,
    save: *mut u8,
}

/// A C `va_list` on Windows: the address of the next argument, each in an 8-byte slot (a
/// `float` promoted to a `double`). The `V` functions receive this address itself.
#[cfg(windows)]
#[repr(C)]
pub(super) struct VaList {
    _first: u64,
}

#[cfg(not(windows))]
impl VaList {
    /// The next integer or pointer argument.
    ///
    /// # Safety
    /// One must remain.
    pub(super) unsafe fn int(&mut self) -> u64 {
        if self.gp_offset < 48 {
            let v = *(self.save.add(self.gp_offset as usize) as *const u64);
            self.gp_offset += 8;
            v
        } else {
            let v = *self.overflow;
            self.overflow = self.overflow.add(1);
            v
        }
    }

    /// The next `double` argument (a `float` was promoted to one), as its bits.
    ///
    /// # Safety
    /// One must remain.
    pub(super) unsafe fn double(&mut self) -> u64 {
        if self.fp_offset < 176 {
            let v = *(self.save.add(self.fp_offset as usize) as *const u64);
            self.fp_offset += 16;
            v
        } else {
            let v = *self.overflow;
            self.overflow = self.overflow.add(1);
            v
        }
    }
}
