//! POSIX calls whose form differs between Linux and macOS, for the natives both class libraries
//! share: descriptors opened close-on-exec, `errno`, memory totals, thread names, `exec`.

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

/// Total and free memory, total and free swap, in bytes (-1 where unknown).
pub(super) fn memory() -> (i64, i64, i64, i64) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: sysinfo fills the struct.
        let mut si: libc::sysinfo = unsafe { std::mem::zeroed() };
        if unsafe { libc::sysinfo(&mut si) } != 0 {
            return (-1, -1, -1, -1);
        }
        let unit = si.mem_unit.max(1) as i64;
        (si.totalram as i64 * unit, si.freeram as i64 * unit, si.totalswap as i64 * unit, si.freeswap as i64 * unit)
    }
    #[cfg(target_os = "macos")]
    {
        let total = crate::os::physical_memory() as i64;
        // Free pages from the VM statistics; swap usage from vm.swapusage.
        let mut free = -1;
        // SAFETY: host_statistics64 fills a vm_statistics64 of the given count.
        unsafe {
            let mut stats: libc::vm_statistics64 = std::mem::zeroed();
            let mut count = (std::mem::size_of::<libc::vm_statistics64>() / std::mem::size_of::<libc::integer_t>()) as libc::mach_msg_type_number_t;
            #[allow(deprecated)]
            let host = libc::mach_host_self();
            if libc::host_statistics64(host, libc::HOST_VM_INFO64, &mut stats as *mut _ as libc::host_info64_t, &mut count) == 0 {
                free = (stats.free_count as i64 + stats.inactive_count as i64) * libc::sysconf(libc::_SC_PAGESIZE) as i64;
            }
        }
        let mut swap: libc::xsw_usage = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::xsw_usage>();
        // SAFETY: sysctlbyname fills an xsw_usage.
        let ok = unsafe { libc::sysctlbyname(c"vm.swapusage".as_ptr(), &mut swap as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) } == 0;
        let (st, sf) = if ok { (swap.xsu_total as i64, swap.xsu_avail as i64) } else { (-1, -1) };
        (total, free, st, sf)
    }
}

/// Names the calling thread (the first 15 bytes, the limit on Linux).
pub(super) fn set_thread_name(name: &std::ffi::CStr) {
    // SAFETY: names the calling thread with a NUL-terminated string.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::pthread_setname_np(libc::pthread_self(), name.as_ptr());
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_setname_np(name.as_ptr());
    }
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
