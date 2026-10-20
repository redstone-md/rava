//! `java.io.WinNTFileSystem`, as the JDK's `WinNTFileSystem_md.c`: attributes, listings and
//! changes through the wide Win32 file calls, and canonical paths with each existing component
//! in the case the file system stores.

use super::io::wide;
use crate::heap::{obj, Oop};
use crate::natives::io_shared::io_error;
use crate::natives::{ret_bool, ret_int, ret_long, ret_ref, ret_void, string_array, NativeRegistry};
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_FILE_EXISTS, FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, DeleteFileW, FindClose, FindFirstFileW, FindNextFileW, GetDiskFreeSpaceExW, GetFileAttributesExW, GetFileAttributesW, GetFinalPathNameByHandleW, GetFullPathNameW,
    GetLogicalDrives, GetVolumeInformationW, GetVolumePathNameW, MoveFileExW, RemoveDirectoryW, SetFileAttributesW, SetFileTime, CREATE_NEW, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, GetFileExInfoStandard, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING, WIN32_FILE_ATTRIBUTE_DATA, WIN32_FIND_DATAW,
};

vm_field!(file_path, "java/io/File", "path", "Ljava/lang/String;");

const FS: &str = "java/io/WinNTFileSystem";
/// `FileSystem.BA_*` and `ACCESS_*`, `SPACE_*`.
const BA_EXISTS: i32 = 0x01;
const BA_REGULAR: i32 = 0x02;
const BA_DIRECTORY: i32 = 0x04;
const BA_HIDDEN: i32 = 0x08;
const ACCESS_WRITE: i32 = 0x02;
/// 100 ns ticks from 1601 to 1970.
const EPOCH_TICKS: i64 = 116_444_736_000_000_000;

fn path(file: Oop) -> String {
    // SAFETY: a live java.io.File.
    rt::java_string_to_rust(unsafe { obj::read_ref(file, file_path()) })
}

fn attributes(p: &str) -> Option<WIN32_FILE_ATTRIBUTE_DATA> {
    let w = wide(p);
    // SAFETY: fills an attribute struct for a NUL-terminated path.
    unsafe {
        let mut d: WIN32_FILE_ATTRIBUTE_DATA = std::mem::zeroed();
        (GetFileAttributesExW(w.as_ptr(), GetFileExInfoStandard, &mut d as *mut _ as *mut _) != 0).then_some(d)
    }
}

fn millis(f: FILETIME) -> i64 {
    ((((f.dwHighDateTime as i64) << 32) | f.dwLowDateTime as i64) - EPOCH_TICKS) / 10_000
}

fn from_wide(w: &[u16]) -> String {
    String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())])
}

/// The full path, `.` and `..` resolved, each existing component as the file system spells it.
fn canonicalize(p: &str) -> Option<String> {
    let w = wide(p);
    let mut full = vec![0u16; 32768];
    // SAFETY: a NUL-terminated path into a buffer of its size.
    let n = unsafe { GetFullPathNameW(w.as_ptr(), full.len() as u32, full.as_mut_ptr(), std::ptr::null_mut()) } as usize;
    if n == 0 || n >= full.len() {
        return None;
    }
    let full = String::from_utf16_lossy(&full[..n]);
    // The root (`C:\` or `\\server\share\`) stays as is; later components take the stored case.
    let parts: Vec<&str> = full.split('\\').collect();
    let root_len = if full.starts_with("\\\\") { 4.min(parts.len()) } else { 1 };
    let mut out = parts[..root_len].join("\\");
    if out.len() == 2 && out.ends_with(':') {
        out = out.to_uppercase();
    }
    for part in &parts[root_len..] {
        if part.is_empty() {
            continue;
        }
        let candidate = format!("{out}\\{part}");
        let cw = wide(&candidate);
        // SAFETY: FindFirstFileW on a path without wildcards (they make no sense here).
        let stored = if part.contains(['*', '?']) {
            None
        } else {
            unsafe {
                let mut fd: WIN32_FIND_DATAW = std::mem::zeroed();
                let h = FindFirstFileW(cw.as_ptr(), &mut fd);
                (h != INVALID_HANDLE_VALUE).then(|| {
                    FindClose(h);
                    from_wide(&fd.cFileName)
                })
            }
        };
        out = format!("{out}\\{}", stored.unwrap_or_else(|| part.to_string()));
    }
    if out.len() == 2 && out.ends_with(':') {
        out.push('\\');
    }
    Some(out)
}

/// The path the file system resolves `p` to, links followed (`GetFinalPathNameByHandleW` on a
/// handle to it), without the `\\?\` prefix; None when it cannot be opened.
fn final_path(p: &str) -> Option<String> {
    let w = wide(p);
    // SAFETY: a handle opened for querying only (directories too), closed after.
    let out = unsafe {
        let h = CreateFileW(w.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut());
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut buf = vec![0u16; 32768];
        let n = GetFinalPathNameByHandleW(h, buf.as_mut_ptr(), buf.len() as u32, 0) as usize;
        CloseHandle(h);
        (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
    }?;
    Some(match (out.strip_prefix("\\\\?\\UNC\\"), out.strip_prefix("\\\\?\\")) {
        (Some(unc), _) => format!("\\\\{unc}"),
        (None, Some(local)) => local.to_string(),
        _ => out,
    })
}

fn list(t: &mut Thread, p: &str) -> JResult<u64> {
    match attributes(p) {
        Some(d) if d.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 => {}
        _ => return ret_ref(std::ptr::null_mut()),
    }
    let pattern = if p.ends_with('\\') || p.ends_with('/') { format!("{p}*") } else { format!("{p}\\*") };
    let w = wide(&pattern);
    let mut names = Vec::new();
    // SAFETY: a find handle walked to its end and closed.
    unsafe {
        let mut fd: WIN32_FIND_DATAW = std::mem::zeroed();
        let h = FindFirstFileW(w.as_ptr(), &mut fd);
        if h != INVALID_HANDLE_VALUE {
            loop {
                let name = from_wide(&fd.cFileName);
                if name != "." && name != ".." {
                    names.push(Some(name));
                }
                if FindNextFileW(h, &mut fd) == 0 {
                    break;
                }
            }
            FindClose(h);
        }
    }
    ret_ref(string_array(t, &names)?)
}

fn set_modified(p: &str, ms: i64) -> bool {
    let w = wide(p);
    let ticks = ms * 10_000 + EPOCH_TICKS;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    // SAFETY: a handle opened for attribute writes, closed after.
    unsafe {
        let h = CreateFileW(w.as_ptr(), FILE_WRITE_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut());
        if h == INVALID_HANDLE_VALUE {
            return false;
        }
        let ok = SetFileTime(h, std::ptr::null(), std::ptr::null(), &ft) != 0;
        CloseHandle(h);
        ok
    }
}

/// Deletes a file or an empty directory; a read-only file only when `read_only_too` (its
/// attribute cleared first, as the JDK does).
fn delete(p: &str, read_only_too: bool) -> bool {
    let w = wide(p);
    let a = attributes(p).map(|d| d.dwFileAttributes);
    if a.is_some_and(|a| a & FILE_ATTRIBUTE_DIRECTORY != 0) {
        // SAFETY: removes a NUL-terminated path.
        return unsafe { RemoveDirectoryW(w.as_ptr()) } != 0;
    }
    if read_only_too && a.is_some_and(|a| a & FILE_ATTRIBUTE_READONLY != 0) {
        set_read_only(p, false);
    }
    // SAFETY: as above.
    unsafe { DeleteFileW(w.as_ptr()) != 0 }
}

fn set_read_only(p: &str, on: bool) -> bool {
    let w = wide(p);
    // SAFETY: attribute calls on a NUL-terminated path.
    unsafe {
        let a = GetFileAttributesW(w.as_ptr());
        if a == INVALID_FILE_ATTRIBUTES {
            return false;
        }
        if a & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return true;
        }
        let new = if on { a | FILE_ATTRIBUTE_READONLY } else { a & !FILE_ATTRIBUTE_READONLY };
        SetFileAttributesW(w.as_ptr(), new) != 0
    }
}

fn space(p: &str, which: i32) -> i64 {
    let w = wide(p);
    let mut root = vec![0u16; 32768];
    let (mut usable, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: the volume of a NUL-terminated path, then its free space.
    unsafe {
        if GetVolumePathNameW(w.as_ptr(), root.as_mut_ptr(), root.len() as u32) == 0 || GetDiskFreeSpaceExW(root.as_ptr(), &mut usable, &mut total, &mut free) == 0 {
            return 0;
        }
    }
    (match which {
        0 => total,
        1 => free,
        _ => usable,
    }) as i64
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(FS, "initIDs", "()V", |_, _| ret_void());
    r.add(FS, "canonicalize0", "(Ljava/lang/String;)Ljava/lang/String;", |t, a| {
        let p = rt::java_string_to_rust(a[1] as Oop);
        match canonicalize(&p) {
            Some(c) => ret_ref(rt::new_string(t, &c)?),
            None => Err(io_error(t, "Bad pathname")),
        }
    });
    // Since 25.0.x: `canonicalize` follows links through this, keeping its own path on failure.
    r.add(FS, "getFinalPath0", "(Ljava/lang/String;)Ljava/lang/String;", |t, a| {
        let p = rt::java_string_to_rust(a[1] as Oop);
        match final_path(&p) {
            Some(f) => ret_ref(rt::new_string(t, &f)?),
            None => Err(io_error(t, "Bad pathname")),
        }
    });
    r.add(FS, "getBooleanAttributes0", "(Ljava/io/File;)I", |_, a| {
        ret_int(attributes(&path(a[1] as Oop)).map_or(0, |d| {
            let dir = d.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
            BA_EXISTS | if dir { BA_DIRECTORY } else { BA_REGULAR } | if d.dwFileAttributes & FILE_ATTRIBUTE_HIDDEN != 0 { BA_HIDDEN } else { 0 }
        }))
    });
    r.add(FS, "checkAccess0", "(Ljava/io/File;I)Z", |_, a| {
        let Some(d) = attributes(&path(a[1] as Oop)) else { return ret_bool(false) };
        let read_only = d.dwFileAttributes & FILE_ATTRIBUTE_READONLY != 0 && d.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0;
        ret_bool(!(a[2] as i32 & ACCESS_WRITE != 0 && read_only))
    });
    r.add(FS, "setPermission0", "(Ljava/io/File;IZZ)Z", |_, a| {
        // Only writability maps onto Windows' read-only attribute; reading and executing are
        // always allowed, so turning them on succeeds and off fails.
        if a[2] as i32 & ACCESS_WRITE != 0 {
            ret_bool(set_read_only(&path(a[1] as Oop), a[3] == 0))
        } else {
            ret_bool(a[3] != 0)
        }
    });
    r.add(FS, "getLastModifiedTime0", "(Ljava/io/File;)J", |_, a| ret_long(attributes(&path(a[1] as Oop)).map_or(0, |d| millis(d.ftLastWriteTime))));
    r.add(FS, "getLength0", "(Ljava/io/File;)J", |_, a| {
        ret_long(attributes(&path(a[1] as Oop)).filter(|d| d.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0).map_or(0, |d| ((d.nFileSizeHigh as i64) << 32) | d.nFileSizeLow as i64))
    });
    r.add(FS, "createFileExclusively0", "(Ljava/lang/String;)Z", |t, a| {
        let w = wide(&rt::java_string_to_rust(a[1] as Oop));
        // SAFETY: creates a new file from a NUL-terminated path, closing its handle.
        let h = unsafe {
            CreateFileW(w.as_ptr(), FILE_GENERIC_READ | FILE_GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(), CREATE_NEW, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut())
        };
        if h == INVALID_HANDLE_VALUE {
            // SAFETY: a plain query.
            return match unsafe { GetLastError() } {
                ERROR_FILE_EXISTS | ERROR_ACCESS_DENIED => ret_bool(false),
                _ => Err(io_error(t, "Could not create file")),
            };
        }
        // SAFETY: the handle just made.
        unsafe { CloseHandle(h) };
        ret_bool(true)
    });
    r.add(FS, "delete0", "(Ljava/io/File;)Z", |_, a| ret_bool(delete(&path(a[1] as Oop), false)));
    // Since 25.0.x, with `jdk.io.File.allowDeleteReadOnlyFiles`.
    r.add(FS, "delete0", "(Ljava/io/File;Z)Z", |_, a| ret_bool(delete(&path(a[1] as Oop), a[2] != 0)));
    r.add(FS, "list0", "(Ljava/io/File;)[Ljava/lang/String;", |t, a| list(t, &path(a[1] as Oop)));
    r.add(FS, "createDirectory0", "(Ljava/io/File;)Z", |_, a| {
        let w = wide(&path(a[1] as Oop));
        // SAFETY: a NUL-terminated path.
        ret_bool(unsafe { CreateDirectoryW(w.as_ptr(), std::ptr::null()) } != 0)
    });
    r.add(FS, "rename0", "(Ljava/io/File;Ljava/io/File;)Z", |_, a| {
        let (from, to) = (wide(&path(a[1] as Oop)), wide(&path(a[2] as Oop)));
        // SAFETY: NUL-terminated paths; an existing target makes it fail, as _wrename does.
        ret_bool(unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0)
    });
    r.add(FS, "setLastModifiedTime0", "(Ljava/io/File;J)Z", |_, a| ret_bool(set_modified(&path(a[1] as Oop), a[2] as i64)));
    r.add(FS, "setReadOnly0", "(Ljava/io/File;)Z", |_, a| ret_bool(set_read_only(&path(a[1] as Oop), true)));
    // The current directory of drive `n` (1 is A:), without the drive.
    r.add(FS, "getDriveDirectory", "(I)Ljava/lang/String;", |t, a| {
        let drive = (b'A' + (a[1] as u8).saturating_sub(1)) as char;
        match canonicalize(&format!("{drive}:.")) {
            Some(p) => ret_ref(rt::new_string(t, p.get(2..).unwrap_or("\\"))?),
            None => ret_ref(std::ptr::null_mut()),
        }
    });
    // SAFETY: a plain query.
    r.add(FS, "listRoots0", "()I", |_, _| ret_int(unsafe { GetLogicalDrives() } as i32));
    r.add(FS, "getSpace0", "(Ljava/io/File;I)J", |_, a| ret_long(space(&path(a[1] as Oop), a[2] as i32)));
    r.add(FS, "getNameMax0", "(Ljava/lang/String;)I", |_, a| {
        let p = rt::java_string_to_rust(a[1] as Oop);
        let w = wide(&p);
        let mut root = vec![0u16; 32768];
        let mut max = 0u32;
        // SAFETY: the volume of a NUL-terminated path, then its limits.
        let ok = unsafe {
            GetVolumePathNameW(w.as_ptr(), root.as_mut_ptr(), root.len() as u32) != 0
                && GetVolumeInformationW(root.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut max, std::ptr::null_mut(), std::ptr::null_mut(), 0) != 0
        };
        ret_int(if ok { max as i32 } else { 255 })
    });
}
