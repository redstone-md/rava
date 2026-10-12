//! Child processes on Windows, as the JDK's `ProcessImpl_md.c`: `CreateProcessW` with the
//! standard handles the caller asks for (a pipe where it passes -1, its end returned in place),
//! waits and exit codes on the process handle, and the `ProcessHandleImpl` natives by pid.

use super::io::wide;
use crate::heap::{obj, Oop};
use crate::natives::io_shared::io_error;
use crate::natives::{ret_bool, ret_int, ret_long, ret_void, NativeRegistry};
use crate::runtime as rt;
use crate::thread::{JResult, Thread};
use windows_sys::Win32::Foundation::{CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS, SYNCHRONIZE};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::*;

const PI: &str = "java/lang/ProcessImpl";
const PH: &str = "java/lang/ProcessHandleImpl";
const STILL_ACTIVE: u32 = 259;

fn inheritable() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: std::ptr::null_mut(), bInheritHandle: 1 }
}

/// The child's end of standard stream `i` (0 in, 1 out, 2 err): the caller's handle, or a new
/// pipe whose other end goes back into `handles[i]`.
fn child_end(t: &mut Thread, handles: &mut [i64], i: usize, opened: &mut Vec<HANDLE>) -> JResult<HANDLE> {
    if handles[i] != -1 {
        return Ok(handles[i] as HANDLE);
    }
    let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    let sa = inheritable();
    // SAFETY: a new pipe; the parent's end is kept out of the child.
    if unsafe { CreatePipe(&mut read, &mut write, &sa, 0) } == 0 {
        return Err(io_error(t, "CreatePipe failed"));
    }
    let (child, parent) = if i == 0 { (read, write) } else { (write, read) };
    unsafe { SetHandleInformation(parent, HANDLE_FLAG_INHERIT, 0) };
    handles[i] = parent as i64;
    opened.push(child);
    Ok(child)
}

/// `create(cmd, envBlock, dir, stdHandles, redirectErrorStream)`: the process handle.
fn create(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let cmd = rt::java_string_to_rust(a[0] as Oop);
    let env = (a[1] != 0).then(|| rt::java_string_to_rust(a[1] as Oop));
    let dir = (a[2] != 0).then(|| rt::java_string_to_rust(a[2] as Oop));
    let handles_arr = a[3] as Oop;
    // SAFETY: a live long[3].
    let mut handles: Vec<i64> = unsafe { obj::array_slice::<i64>(handles_arr) }.to_vec();
    let mut opened = Vec::new();
    let stdin = child_end(t, &mut handles, 0, &mut opened)?;
    let stdout = child_end(t, &mut handles, 1, &mut opened)?;
    let stderr = if a[4] != 0 { stdout } else { child_end(t, &mut handles, 2, &mut opened)? };
    // SAFETY: an all-zero STARTUPINFOW is valid; the fields that matter are set below.
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdInput = stdin;
    si.hStdOutput = stdout;
    si.hStdError = stderr;
    let mut cmd_w = wide(&cmd);
    let env_w: Option<Vec<u16>> = env.map(|e| e.encode_utf16().chain([0, 0]).collect());
    let dir_w = dir.map(|d| wide(&d));
    // SAFETY: an all-zero PROCESS_INFORMATION is filled by the call.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated wide strings and inheritable standard handles.
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd_w.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            env_w.as_ref().map_or(std::ptr::null(), |e| e.as_ptr() as *const _),
            dir_w.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            &si,
            &mut pi,
        )
    } != 0;
    let error = crate::natives::io_shared::errno_text();
    for h in opened {
        // SAFETY: the child's ends, inherited by now (or unused).
        unsafe { CloseHandle(h) };
    }
    if !ok {
        return Err(io_error(t, &format!("CreateProcess error ({error})")));
    }
    // SAFETY: the thread handle is not needed; the long[3] receives the parent's ends.
    unsafe {
        CloseHandle(pi.hThread);
        obj::array_slice::<i64>(handles_arr).copy_from_slice(&handles);
    }
    ret_long(pi.hProcess as i64)
}

fn exit_code(h: HANDLE) -> u32 {
    let mut code = 0u32;
    // SAFETY: a process handle.
    unsafe { GetExitCodeProcess(h, &mut code) };
    code
}

/// Waits for `h` up to `millis` (`INFINITE` for no limit) outside Java.
fn wait(t: &mut Thread, h: HANDLE, millis: u32) -> u32 {
    let hv = h as usize;
    // SAFETY: a process handle.
    crate::gc::safepoint::blocking(t, || unsafe { WaitForSingleObject(hv as HANDLE, millis) })
}

/// A handle on process `pid` for queries and waits, closed when dropped.
struct Opened(HANDLE);

impl Opened {
    fn new(pid: i64, access: PROCESS_ACCESS_RIGHTS) -> Option<Opened> {
        // SAFETY: opens by pid; null when there is no such process.
        let h = unsafe { OpenProcess(access, 0, pid as u32) };
        (!h.is_null()).then_some(Opened(h))
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: the handle `new` opened.
        unsafe { CloseHandle(self.0) };
    }
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(PI, "create", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[JZ)J", create);
    r.add(PI, "closeHandle", "(J)Z", |_, a| ret_bool(unsafe { CloseHandle(a[0] as HANDLE) } != 0));
    r.add(PI, "getExitCodeProcess", "(J)I", |_, a| ret_int(exit_code(a[0] as HANDLE) as i32));
    r.add(PI, "getStillActive", "()I", |_, _| ret_int(STILL_ACTIVE as i32));
    r.add(PI, "isProcessAlive", "(J)Z", |_, a| ret_bool(exit_code(a[0] as HANDLE) == STILL_ACTIVE));
    // SAFETY: a process handle.
    r.add(PI, "getProcessId0", "(J)I", |_, a| ret_int(unsafe { GetProcessId(a[0] as HANDLE) } as i32));
    r.add(PI, "terminateProcess", "(J)V", |_, a| {
        // SAFETY: a process handle.
        unsafe { TerminateProcess(a[0] as HANDLE, 1) };
        ret_void()
    });
    r.add(PI, "waitForInterruptibly", "(J)V", |t, a| {
        wait(t, a[0] as HANDLE, INFINITE);
        ret_void()
    });
    r.add(PI, "waitForTimeoutInterruptibly", "(JJ)V", |t, a| {
        wait(t, a[0] as HANDLE, (a[2] as i64).clamp(0, (INFINITE - 1) as i64) as u32);
        ret_void()
    });
    r.add(PI, "openForAtomicAppend", "(Ljava/lang/String;)J", |t, a| {
        let w = wide(&rt::java_string_to_rust(a[0] as Oop));
        // SAFETY: a NUL-terminated path, opened for appending.
        let h = unsafe { CreateFileW(w.as_ptr(), FILE_APPEND_DATA | SYNCHRONIZE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(), OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut()) };
        if h == INVALID_HANDLE_VALUE {
            return Err(io_error(t, "CreateFileW"));
        }
        ret_long(h as i64)
    });
    r.add(PH, "initNative", "()V", |_, _| ret_void());
    r.add(PH, "getCurrentPid0", "()J", |_, _| ret_long(std::process::id() as i64));
    // 0 while alive (no start time kept), -1 once gone.
    r.add(PH, "isAlive0", "(J)J", |_, a| {
        let alive = Opened::new(a[0] as i64, PROCESS_QUERY_LIMITED_INFORMATION).is_some_and(|p| exit_code(p.0) == STILL_ACTIVE);
        ret_long(if alive { 0 } else { -1 })
    });
    r.add(PH, "waitForProcessExit0", "(JZ)I", |t, a| {
        let Some(p) = Opened::new(a[0] as i64, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE) else { return ret_int(0) };
        if wait(t, p.0, INFINITE) != WAIT_OBJECT_0 {
            return ret_int(0);
        }
        ret_int(exit_code(p.0) as i32)
    });
    r.add(PH, "destroy0", "(JJZ)Z", |_, a| {
        // SAFETY: a process handle opened for termination.
        ret_bool(Opened::new(a[0] as i64, PROCESS_TERMINATE).is_some_and(|p| unsafe { TerminateProcess(p.0, 1) } != 0))
    });
    r.add(PH, "parent0", "(JJ)J", |_, _| ret_long(-1));
    r.add(PH, "getProcessPids0", "(J[J[J[J)I", |_, _| ret_int(0));
    r.add("java/lang/ProcessHandleImpl$Info", "initIDs", "()V", |_, _| ret_void());
    r.add("java/lang/ProcessHandleImpl$Info", "info0", "(J)V", |_, _| ret_void());
}
