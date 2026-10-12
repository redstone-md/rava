//! Child processes: `java.lang.ProcessImpl.forkAndExec` and the `ProcessHandleImpl` natives the
//! reaper thread and `Process` use (waiting, liveness, destruction, parents).
//!
//! The child is forked and execs the program itself; between the two it only makes
//! async-signal-safe calls (everything it needs is prepared before the fork). An exec failure
//! is reported to the parent through a close-on-exec pipe, as the JDK's launcher does.

use super::{ret_bool, ret_int, ret_long, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime::throw_new;
use crate::thread::{JResult, Thread};
use std::ffi::CString;

const PI: &str = "java/lang/ProcessImpl";
const PH: &str = "java/lang/ProcessHandleImpl";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(PI, "init", "()V", |_, _| ret_void());
    r.add(PI, "forkAndExec", "(I[B[B[BI[BI[B[IZ)I", fork_and_exec);
    r.add(PH, "initNative", "()V", |_, _| ret_void());
    r.add(PH, "getCurrentPid0", "()J", |_, _| ret_long(std::process::id() as i64));
    r.add(PH, "isAlive0", "(J)J", |_, a| ret_long(if unsafe { libc::kill(a[0] as i32, 0) } == 0 { 0 } else { -1 }));
    r.add(PH, "waitForProcessExit0", "(JZ)I", |t, a| {
        let (pid, reap) = (a[0] as i32, a[2] != 0);
        ret_int(crate::gc::safepoint::blocking(t, || wait_exit(pid, reap)))
    });
    r.add(PH, "destroy0", "(JJZ)Z", |_, a| {
        let sig = if a[4] != 0 { libc::SIGKILL } else { libc::SIGTERM };
        ret_bool(unsafe { libc::kill(a[0] as i32, sig) } == 0)
    });
    r.add(PH, "parent0", "(JJ)J", |_, a| ret_long(parent_of(a[0] as i32).unwrap_or(-1)));
    r.add(PH, "getProcessPids0", "(J[J[J[J)I", |_, _| ret_int(0));
    r.add("java/lang/ProcessHandleImpl$Info", "initIDs", "()V", |_, _| ret_void());
    r.add("java/lang/ProcessHandleImpl$Info", "info0", "(J)V", |_, _| ret_void());
}

/// Blocks until `pid` exits; its exit status (0x80 + signal when killed). Without `reap` the
/// child stays waitable.
fn wait_exit(pid: i32, reap: bool) -> i32 {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | if reap { 0 } else { libc::WNOWAIT };
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } == -1 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return 0;
        }
        // SAFETY: filled by a successful waitid.
        let status = unsafe { info.si_status() };
        return if info.si_code == libc::CLD_EXITED { status } else { 0x80 + status };
    }
}

fn parent_of(pid: i32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // "pid (comm) state ppid ...": comm may contain spaces, so parse after its closing paren.
    stat[stat.rfind(')')? + 1..].split_whitespace().nth(1)?.parse().ok()
}

/// The bytes of a byte[], or `None` for null.
fn bytes(o: u64) -> Option<Vec<u8>> {
    // SAFETY: a live byte[] (or null).
    (o != 0).then(|| unsafe { obj::array_slice::<u8>(o as Oop).to_vec() })
}

/// A NUL-terminated C string from a byte[] that may carry its own terminator.
fn c_string(b: &[u8]) -> CString {
    CString::new(b.split(|&c| c == 0).next().unwrap_or(&[])).expect("no interior NUL")
}

/// `count` NUL-separated strings of a block.
fn c_strings(block: &[u8], count: usize) -> Vec<CString> {
    block.split(|&c| c == 0).take(count).map(|s| CString::new(s).expect("no interior NUL")).collect()
}

fn fork_and_exec(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    // a: this, mode, helperpath, prog, argBlock, argc, envBlock, envc, dir, fds, redirectErrorStream
    let prog = c_string(&bytes(a[3]).unwrap_or_default());
    let mut argv = vec![prog.clone()];
    argv.extend(c_strings(&bytes(a[4]).unwrap_or_default(), a[5] as usize));
    let env = bytes(a[6]).map(|b| c_strings(&b, a[7] as usize));
    let dir = bytes(a[8]).map(|b| c_string(&b));
    let fds_arr = a[9] as Oop;
    let redirect = a[10] != 0;
    // SAFETY: the int[3] of stdin/stdout/stderr descriptors (-1: make a pipe).
    let fds = unsafe { obj::array_slice::<i32>(fds_arr) };
    let argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|s| s.as_ptr()).chain(std::iter::once(std::ptr::null())).collect();
    let env_ptrs: Option<Vec<*const libc::c_char>> = env.as_ref().map(|e| e.iter().map(|s| s.as_ptr()).chain(std::iter::once(std::ptr::null())).collect());
    // Pipes for the streams the parent keeps: child end, parent end.
    let mut child_fd = [-1i32; 3];
    let mut parent_fd = [-1i32; 3];
    for i in 0..3 {
        if i == 2 && redirect {
            continue;
        }
        if fds[i] != -1 {
            child_fd[i] = fds[i];
            continue;
        }
        let mut p = [0i32; 2];
        if super::posix::pipe(&mut p) == -1 {
            close_all(&parent_fd);
            return Err(exec_error(t, errno()));
        }
        // stdin: the child reads; stdout/stderr: the child writes.
        let (c, p_end) = if i == 0 { (p[0], p[1]) } else { (p[1], p[0]) };
        child_fd[i] = c;
        parent_fd[i] = p_end;
    }
    let mut fail = [0i32; 2];
    if super::posix::pipe(&mut fail) == -1 {
        close_all(&parent_fd);
        close_all(&child_fd);
        return Err(exec_error(t, errno()));
    }
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // Child: only async-signal-safe calls from here.
        unsafe {
            for (i, &fd) in child_fd.iter().enumerate() {
                if fd != -1 {
                    libc::dup2(fd, i as i32);
                }
            }
            if redirect {
                libc::dup2(1, 2);
            }
            // Inherited descriptors (other than 0-2 and the failure pipe) close on exec.
            super::posix::cloexec_from_3();
            if let Some(d) = &dir {
                if libc::chdir(d.as_ptr()) == -1 {
                    child_fail(fail[1]);
                }
            }
            match &env_ptrs {
                Some(e) => super::posix::execvpe(prog.as_ptr(), argv_ptrs.as_ptr(), e.as_ptr()),
                None => {
                    libc::execvp(prog.as_ptr(), argv_ptrs.as_ptr());
                }
            };
            child_fail(fail[1]);
        }
    }
    let err = errno();
    unsafe { libc::close(fail[1]) };
    for (i, &fd) in child_fd.iter().enumerate() {
        // Descriptors the caller passed in stay open; the pipe ends the child uses close here.
        if fd != -1 && fds[i] == -1 {
            unsafe { libc::close(fd) };
        }
    }
    if pid == -1 {
        unsafe { libc::close(fail[0]) };
        close_all(&parent_fd);
        return Err(exec_error(t, err));
    }
    // The child's errno if it failed before or at exec; nothing when exec succeeded.
    let mut child_errno = 0i32;
    let n = crate::gc::safepoint::blocking(t, || loop {
        let n = unsafe { libc::read(fail[0], &mut child_errno as *mut i32 as *mut libc::c_void, 4) };
        if n != -1 || errno() != libc::EINTR {
            return n;
        }
    });
    unsafe { libc::close(fail[0]) };
    if n == 4 {
        crate::gc::safepoint::blocking(t, || wait_exit(pid, true));
        close_all(&parent_fd);
        return Err(exec_error(t, child_errno));
    }
    // SAFETY: the same int[3], still live.
    let fds = unsafe { obj::array_slice::<i32>(fds_arr) };
    for i in 0..3 {
        fds[i] = parent_fd[i];
    }
    ret_int(pid)
}

/// In the child: reports errno to the parent and exits.
unsafe fn child_fail(fd: i32) -> ! {
    let e = errno();
    unsafe {
        libc::write(fd, &e as *const i32 as *const libc::c_void, 4);
        libc::_exit(127)
    }
}

fn close_all(fds: &[i32]) {
    for &fd in fds.iter().filter(|&&fd| fd != -1) {
        unsafe { libc::close(fd) };
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `IOException("error=N, message")`, as the JDK's launcher reports a failed start.
fn exec_error(t: &mut Thread, err: i32) -> crate::thread::Throw {
    let msg = std::io::Error::from_raw_os_error(err).to_string();
    let text = msg.split(" (os error").next().unwrap_or(&msg);
    throw_new(t, "java/io/IOException", &format!("error={err}, {text}"))
}
