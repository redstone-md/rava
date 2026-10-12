//! POSIX calls whose form differs between Linux and macOS, for the natives both class libraries
//! share: descriptors opened close-on-exec, `errno`, socket addresses, `exec`.

/// The calling thread's `errno`.
pub(super) fn errno_ptr() -> *mut libc::c_int {
    // SAFETY: the C library's thread-local errno.
    #[cfg(target_os = "linux")]
    return unsafe { libc::__errno_location() };
    #[cfg(target_os = "macos")]
    return unsafe { libc::__error() };
}

#[cfg(not(target_os = "linux"))]
fn cloexec(fd: libc::c_int) {
    // SAFETY: flags of a descriptor the caller just made.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
}

/// `pipe` with both ends close-on-exec; 0 or -1 with errno set.
pub(super) fn pipe(fds: &mut [i32; 2]) -> i32 {
    #[cfg(target_os = "linux")]
    // SAFETY: fills two descriptors.
    return unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: fills two descriptors.
        let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if r == 0 {
            fds.iter().for_each(|&f| cloexec(f));
        }
        r
    }
}

/// `socket` close-on-exec.
pub(super) fn socket(domain: i32, ty: i32) -> i32 {
    #[cfg(target_os = "linux")]
    // SAFETY: a plain socket call.
    return unsafe { libc::socket(domain, ty | libc::SOCK_CLOEXEC, 0) };
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: a plain socket call.
        let s = unsafe { libc::socket(domain, ty, 0) };
        if s >= 0 {
            cloexec(s);
        }
        s
    }
}

/// `accept` close-on-exec.
pub(super) fn accept(fd: i32, addr: *mut libc::sockaddr, len: *mut libc::socklen_t) -> i32 {
    #[cfg(target_os = "linux")]
    // SAFETY: the caller's buffers.
    return unsafe { libc::accept4(fd, addr, len, libc::SOCK_CLOEXEC) };
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: the caller's buffers.
        let s = unsafe { libc::accept(fd, addr, len) };
        if s >= 0 {
            cloexec(s);
        }
        s
    }
}

/// An IPv4 socket address.
pub(super) fn sockaddr_in(ip: u32, port: u16) -> libc::sockaddr_in {
    // SAFETY: all-zero is a valid sockaddr_in; the fields that matter are set below.
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_port = port.to_be();
    sa.sin_addr = libc::in_addr { s_addr: ip.to_be() };
    #[cfg(target_os = "macos")]
    {
        sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    }
    sa
}

/// In a forked child: marks every descriptor from 3 up close-on-exec.
pub(super) unsafe fn cloexec_from_3() {
    #[cfg(target_os = "linux")]
    {
        libc::syscall(libc::SYS_close_range, 3, u32::MAX, 4 /* CLOSE_RANGE_CLOEXEC */);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let max = libc::getdtablesize();
        for fd in 3..max {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
}

extern "C" {
    #[cfg(target_os = "macos")]
    static mut environ: *const *const libc::c_char;
}

/// In a forked child: `execvp` with environment `env` (async-signal-safe on both systems).
pub(super) unsafe fn execvpe(prog: *const libc::c_char, argv: *const *const libc::c_char, env: *const *const libc::c_char) {
    #[cfg(target_os = "linux")]
    {
        libc::execvpe(prog, argv, env);
    }
    #[cfg(target_os = "macos")]
    {
        environ = env;
        libc::execvp(prog, argv);
    }
}
