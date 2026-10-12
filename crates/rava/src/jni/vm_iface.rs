//! The `JavaVM` invocation interface native code receives in `JNI_OnLoad` and through
//! `GetJavaVM`: one VM, whose `GetEnv` and `AttachCurrentThread` answer with the calling
//! thread's `JNIEnv`. Threads the VM did not start are not attached yet (`JNI_EDETACHED`).

use std::ffi::c_void;

const JNI_OK: i32 = 0;
const JNI_EDETACHED: i32 = -2;
const JNI_EVERSION: i32 = -3;

#[repr(C)]
struct JavaVm {
    functions: *const [usize; 8],
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
            attach_current_thread as *const () as usize,
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
    if !(0x0001_0001..=super::VERSION).contains(&version) {
        return JNI_EVERSION;
    }
    match current_env() {
        Some(env) => {
            // SAFETY: the caller's out-parameter.
            unsafe { *penv = env };
            JNI_OK
        }
        None => JNI_EDETACHED,
    }
}

extern "C" fn attach_current_thread(_vm: *const c_void, penv: *mut *mut c_void, _args: *mut c_void) -> i32 {
    match current_env() {
        Some(env) => {
            // SAFETY: the caller's out-parameter.
            unsafe { *penv = env };
            JNI_OK
        }
        None => {
            eprintln!("[rava] JNI: AttachCurrentThread from a thread the VM did not start is not supported yet");
            JNI_EDETACHED
        }
    }
}

extern "C" fn detach_current_thread(_vm: *const c_void) -> i32 {
    JNI_OK
}

extern "C" fn destroy_java_vm(_vm: *const c_void) -> i32 {
    -1
}
