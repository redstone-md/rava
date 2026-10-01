//! `java.io.UnixFileSystem` natives over the host filesystem.

use super::{ret_bool, ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

vm_field!(file_path, "java/io/File", "path", "Ljava/lang/String;");

const UFS: &str = "java/io/UnixFileSystem";
const BA_EXISTS: i32 = 0x01;
const BA_REGULAR: i32 = 0x02;
const BA_DIRECTORY: i32 = 0x04;
const BA_HIDDEN: i32 = 0x08;

fn path_of(file: Oop) -> String {
    // SAFETY: live java.io.File.
    rt::java_string_to_rust(unsafe { obj::read::<Oop>(file, file_path()) })
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(UFS, "initIDs", "()V", |_, _| ret_void());
    r.add(UFS, "canonicalize0", "(Ljava/lang/String;)Ljava/lang/String;", |t, a| {
        let p = rt::java_string_to_rust(a[1] as Oop);
        let c = canonicalize(Path::new(&p));
        ret_ref(rt::new_string(t, &c.to_string_lossy())?)
    });
    r.add(UFS, "getBooleanAttributes0", "(Ljava/io/File;)I", |_, a| {
        let p = path_of(a[1] as Oop);
        let Ok(m) = fs::metadata(&p) else { return ret_int(0) };
        let hidden = Path::new(&p).file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
        ret_int(
            BA_EXISTS
                | if m.is_file() { BA_REGULAR } else { 0 }
                | if m.is_dir() { BA_DIRECTORY } else { 0 }
                | if hidden { BA_HIDDEN } else { 0 },
        )
    });
    r.add(UFS, "checkAccess0", "(Ljava/io/File;I)Z", |_, a| {
        let c = std::ffi::CString::new(path_of(a[1] as Oop)).unwrap_or_default();
        let mode = ((a[2] as i32 & 4 != 0) as i32 * libc::R_OK)
            | ((a[2] as i32 & 2 != 0) as i32 * libc::W_OK)
            | ((a[2] as i32 & 1 != 0) as i32 * libc::X_OK);
        // SAFETY: NUL-terminated path.
        ret_bool(unsafe { libc::access(c.as_ptr(), mode) } == 0)
    });
    r.add(UFS, "getLastModifiedTime0", "(Ljava/io/File;)J", |_, a| {
        let m = fs::metadata(path_of(a[1] as Oop));
        ret_long(m.map_or(0, |m| m.mtime() * 1000 + m.mtime_nsec() / 1_000_000))
    });
    r.add(UFS, "getLength0", "(Ljava/io/File;)J", |_, a| ret_long(fs::metadata(path_of(a[1] as Oop)).map_or(0, |m| m.len() as i64)));
    r.add(UFS, "setPermission0", "(Ljava/io/File;IZZ)Z", |_, a| {
        let p = path_of(a[1] as Oop);
        let Ok(m) = fs::metadata(&p) else { return ret_bool(false) };
        let bits = (a[2] as u32 & 7) * if a[4] != 0 { 0o100 } else { 0o111 };
        let mode = m.permissions().mode();
        let mode = if a[3] != 0 { mode | bits } else { mode & !bits };
        ret_bool(fs::set_permissions(&p, fs::Permissions::from_mode(mode)).is_ok())
    });
    r.add(UFS, "createFileExclusively0", "(Ljava/lang/String;)Z", |_, a| {
        let p = rt::java_string_to_rust(a[1] as Oop);
        ret_bool(fs::OpenOptions::new().write(true).create_new(true).open(p).is_ok())
    });
    r.add(UFS, "delete0", "(Ljava/io/File;)Z", |_, a| {
        let p = path_of(a[1] as Oop);
        ret_bool(fs::remove_file(&p).is_ok() || fs::remove_dir(&p).is_ok())
    });
    r.add(UFS, "list0", "(Ljava/io/File;)[Ljava/lang/String;", list);
    r.add(UFS, "createDirectory0", "(Ljava/io/File;)Z", |_, a| ret_bool(fs::create_dir(path_of(a[1] as Oop)).is_ok()));
    r.add(UFS, "rename0", "(Ljava/io/File;Ljava/io/File;)Z", |_, a| {
        ret_bool(fs::rename(path_of(a[1] as Oop), path_of(a[2] as Oop)).is_ok())
    });
    r.add(UFS, "setLastModifiedTime0", "(Ljava/io/File;J)Z", |_, _| ret_bool(false));
    r.add(UFS, "setReadOnly0", "(Ljava/io/File;)Z", |_, a| {
        let p = path_of(a[1] as Oop);
        let Ok(m) = fs::metadata(&p) else { return ret_bool(false) };
        let mode = m.permissions().mode() & !0o222;
        ret_bool(fs::set_permissions(&p, fs::Permissions::from_mode(mode)).is_ok())
    });
    r.add(UFS, "getSpace0", "(Ljava/io/File;I)J", |_, _| ret_long(0));
    r.add(UFS, "getNameMax0", "(Ljava/lang/String;)J", |_, _| ret_long(255));
}

fn list(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let Ok(dir) = fs::read_dir(path_of(a[1] as Oop)) else { return ret_ref(std::ptr::null_mut()) };
    let names: Vec<Option<String>> = dir.filter_map(|e| e.ok()).map(|e| Some(e.file_name().to_string_lossy().into_owned())).collect();
    ret_ref(super::string_array(t, &names)?)
}

/// Java's canonical form: resolve the longest existing prefix, keep the rest normalized.
fn canonicalize(p: &Path) -> PathBuf {
    if let Ok(c) = fs::canonicalize(p) {
        return c;
    }
    let mut normalized = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            c => normalized.push(c),
        }
    }
    let mut prefix = normalized.clone();
    let mut tail = Vec::new();
    while let Some(name) = prefix.file_name().map(|n| n.to_owned()) {
        prefix.pop();
        tail.push(name);
        if let Ok(c) = fs::canonicalize(&prefix) {
            return tail.iter().rev().fold(c, |acc, n| acc.join(n));
        }
    }
    normalized
}
