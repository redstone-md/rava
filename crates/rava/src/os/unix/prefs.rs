//! File permissions and process-wide POSIX record locks for the Unix preferences backend.

use std::ffi::CStr;

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

pub(crate) fn prefs_chmod(path: &CStr, mode: u32) -> i32 {
    // SAFETY: a NUL-terminated path, with the mode supplied by the class library.
    if unsafe { libc::chmod(path.as_ptr(), mode as libc::mode_t) } == 0 { 0 } else { errno() }
}

/// Returns a nonzero lock cookie and zero, or zero and the system error.
pub(crate) fn prefs_lock(path: &CStr, mode: u32, shared: bool) -> (i32, i32) {
    // SAFETY: open reads the NUL-terminated path; fchmod and close use descriptors owned here.
    let mut fd = unsafe {
        if shared {
            libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC)
        } else {
            let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC, mode as libc::c_uint);
            if fd >= 0 {
                // Apply the requested creation mode without changing the process-wide umask.
                if libc::fchmod(fd, mode as libc::mode_t) != 0 {
                    let error = errno();
                    libc::close(fd);
                    return (0, error);
                }
                fd
            } else if errno() == libc::EEXIST {
                // Do not change the permissions of an existing lock file.
                libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC)
            } else {
                return (0, errno());
            }
        }
    };
    if fd < 0 { return (0, errno()); }
    if fd == 0 {
        // Java reserves cookie zero for failure, even when stdin was closed before open.
        // SAFETY: duplicate our descriptor with close-on-exec, then close the original.
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 1) };
        let error = if duplicate < 0 { errno() } else { 0 };
        // SAFETY: close consumes the original descriptor acquired above, not the duplicate.
        unsafe { libc::close(fd) };
        if duplicate < 0 { return (0, error); }
        fd = duplicate;
    }
    let kind = if shared { libc::F_RDLCK } else { libc::F_WRLCK };
    let error = record_lock(fd, kind as i32);
    if error != 0 {
        // SAFETY: no lock was acquired; close the descriptor while retaining fcntl's error.
        unsafe { libc::close(fd) };
        (0, error)
    } else {
        (fd, 0)
    }
}

fn record_lock(fd: i32, kind: i32) -> i32 {
    // SAFETY: a zeroed flock, with the requested lock spanning the whole file from offset zero.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind as _;
    lock.l_whence = libc::SEEK_SET as _;
    // SAFETY: fcntl receives an initialized flock valid for this call and does not retain it.
    if unsafe { libc::fcntl(fd, libc::F_SETLK, &lock) } == 0 { 0 } else { errno() }
}

pub(crate) fn prefs_unlock(fd: i32) -> i32 {
    let error = record_lock(fd, libc::F_UNLCK as i32);
    // SAFETY: the cookie is the descriptor returned by prefs_lock, consumed on either path.
    let closed = unsafe { libc::close(fd) };
    if error != 0 { error } else if closed != 0 { errno() } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

    #[test]
    fn preferences_permissions_locks_and_errno() {
        let dir = std::env::temp_dir().join(format!("rava-prefs-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("lock");
        let path = std::ffi::CString::new(file.as_os_str().as_bytes()).unwrap();
        let (fd, error) = prefs_lock(&path, 0o640, false);
        assert!(fd > 0);
        assert_eq!(error, 0);
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
        // SAFETY: queries the live descriptor this test acquired.
        assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
        assert_eq!(prefs_chmod(&path, 0o600), 0);
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(prefs_unlock(fd), 0);
        let (fd, error) = prefs_lock(&path, 0o777, true);
        assert!(fd > 0);
        assert_eq!(error, 0);
        assert_eq!(prefs_unlock(fd), 0);
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_file(file).unwrap();
        assert_eq!(prefs_chmod(&path, 0o600), libc::ENOENT);
        assert_eq!(prefs_lock(&path, 0o600, true), (0, libc::ENOENT));
        std::fs::remove_dir(dir).unwrap();
    }
}
