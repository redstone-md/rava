//! The `JavaVM` invocation interface native code receives in `JNI_OnLoad` and through
//! `GetJavaVM`: one VM, whose `GetEnv` answers with the calling thread's `JNIEnv`. A thread the
//! VM did not start (a driver's callback thread, say) becomes a Java thread through
//! `AttachCurrentThread`, until `DetachCurrentThread` or its end.

use crate::gc::safepoint::{self, NATIVE};
use crate::heap::Oop;
use crate::thread::Thread;
use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::Ordering::SeqCst;

const JNI_OK: i32 = 0;
const JNI_ERR: i32 = -1;
const JNI_EDETACHED: i32 = -2;
const JNI_EVERSION: i32 = -3;

#[repr(C)]
struct JavaVm {
    functions: *const [usize; 8],
}

/// `JavaVMAttachArgs`: every field optional.
#[repr(C)]
struct AttachArgs {
    version: i32,
    name: *const c_char,
    group: Oop,
}

struct Shared(JavaVm);
// SAFETY: the table it points at is immutable and lives for the process.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

/// The process's `JavaVM*`.
pub(crate) fn java_vm() -> *const c_void {
    static VM: std::sync::OnceLock<(Box<[usize; 8]>, Shared)> = std::sync::OnceLock::new();
    let (_, vm) = VM.get_or_init(|| {
        let table = Box::new([
            0,
            0,
            0,
            destroy_java_vm as *const () as usize,
            attach_current_thread as *const () as usize,
            detach_current_thread as *const () as usize,
            get_env as *const () as usize,
            attach_current_thread_as_daemon as *const () as usize,
        ]);
        let vm = Shared(JavaVm { functions: &*table as *const [usize; 8] });
        (table, vm)
    });
    &vm.0 as *const JavaVm as *const c_void
}

/// The calling thread's environment, if it is a Java thread.
fn current_env() -> Option<*mut c_void> {
    let t = super::current();
    (!t.is_null()).then(|| {
        // SAFETY: the current thread's own `Thread`, alive while it runs.
        super::env_of(unsafe { &mut *t }) as *mut c_void
    })
}

extern "C" fn get_env(_vm: *const c_void, penv: *mut *mut c_void, version: i32) -> i32 {
    // SAFETY: the caller's out-parameter, null when the thread is not attached (callers rely
    // on it: LWJGL attaches when it reads back null).
    unsafe { *penv = std::ptr::null_mut() };
    if !(0x0001_0001..=super::VERSION).contains(&version) {
        return JNI_EVERSION;
    }
    match current_env() {
        Some(env) => {
            // SAFETY: as above.
            unsafe { *penv = env };
            JNI_OK
        }
        None => JNI_EDETACHED,
    }
}

extern "C" fn attach_current_thread(_vm: *const c_void, penv: *mut *mut c_void, args: *mut c_void) -> i32 {
    attach(penv, args as *const AttachArgs, false)
}

extern "C" fn attach_current_thread_as_daemon(_vm: *const c_void, penv: *mut *mut c_void, args: *mut c_void) -> i32 {
    attach(penv, args as *const AttachArgs, true)
}

/// A thread the VM did not start, attached: its `Thread` lives until it detaches, or until
/// the thread ends (this value is then dropped as a thread-local, and unregisters it so no
/// collection scans a stack that is gone).
struct Adopted(*mut Thread);

impl Adopted {
    /// Unregisters the thread; with `exit`, after running `Thread.exit()` as a detach does.
    fn end(&mut self, exit: bool) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: the `Thread` leaked for this thread in `attach`, freed only here.
        let t = unsafe { &mut *std::mem::replace(&mut self.0, std::ptr::null_mut()) };
        if let Some(rec) = t.gc_record.filter(|r| r.state.load(SeqCst) == NATIVE) {
            safepoint::leave_native(rec);
        }
        crate::natives::thread::release(t, exit);
        t.detach();
        super::set_current(std::ptr::null_mut());
        // SAFETY: unregistered: nothing refers to it any more.
        drop(unsafe { Box::from_raw(t) });
    }
}

impl Drop for Adopted {
    fn drop(&mut self) {
        self.end(false);
    }
}

thread_local! {
    static ADOPTED: RefCell<Option<Adopted>> = const { RefCell::new(None) };
}

fn attach(penv: *mut *mut c_void, args: *const AttachArgs, daemon: bool) -> i32 {
    if let Some(env) = current_env() {
        // SAFETY: the caller's out-parameter.
        unsafe { *penv = env };
        return JNI_OK;
    }
    // SAFETY: optional, and then a `JavaVMAttachArgs` with an optional name and group.
    let (name, group) = match unsafe { args.as_ref() } {
        Some(a) => (
            (!a.name.is_null()).then(|| unsafe { CStr::from_ptr(a.name) }.to_string_lossy().into_owned()),
            (!a.group.is_null()).then_some(a.group),
        ),
        None => (None, None),
    };
    let Some(group) = group.or_else(crate::boot::main_thread_group) else {
        return JNI_ERR;
    };
    let t: &'static mut Thread = Box::leak(Box::new(Thread::new()));
    t.attach();
    let mut adopted = Adopted(t as *mut Thread);
    if crate::natives::thread::adopt(t, group, name.as_deref(), daemon).is_err() {
        adopted.end(false);
        return JNI_ERR;
    }
    // Back in native code: collections do not wait for it.
    if let Some(rec) = t.gc_record {
        safepoint::enter_native(rec);
    }
    ADOPTED.with(|a| *a.borrow_mut() = Some(adopted));
    // SAFETY: the caller's out-parameter.
    unsafe { *penv = super::env_of(t) as *mut c_void };
    JNI_OK
}

/// Detaches a thread `AttachCurrentThread` attached; the VM's own threads stay as they are.
extern "C" fn detach_current_thread(_vm: *const c_void) -> i32 {
    if let Some(mut adopted) = ADOPTED.with(|a| a.borrow_mut().take()) {
        adopted.end(true);
    }
    JNI_OK
}

extern "C" fn destroy_java_vm(_vm: *const c_void) -> i32 {
    -1
}
