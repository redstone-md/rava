//! `com.sun.security.auth.module.UnixSystem`: the process's user and groups, for JAAS.

use super::{ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime::{self as rt, vm_field};
use crate::types::BasicType;
use crate::vm::vm;

const U: &str = "com/sun/security/auth/module/UnixSystem";
vm_field!(username_offset, "com/sun/security/auth/module/UnixSystem", "username", "Ljava/lang/String;");
vm_field!(uid_offset, "com/sun/security/auth/module/UnixSystem", "uid", "J");
vm_field!(gid_offset, "com/sun/security/auth/module/UnixSystem", "gid", "J");
vm_field!(groups_offset, "com/sun/security/auth/module/UnixSystem", "groups", "[J");

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(U, "getUnixInfo", "()V", |t, a| {
        let this = a[0] as Oop;
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let groups = supplementary_groups();
        let longs = vm().primitive_array_class(BasicType::Long).expect("[J");
        let arr = rt::alloc_array(t, longs, groups.len() as i32)?;
        // SAFETY: a fresh long[] of the same length.
        unsafe { obj::array_slice::<i64>(arr).iter_mut().zip(&groups).for_each(|(d, &g)| *d = g as i64) };
        let name = match user_name(uid) {
            Some(n) => rt::new_string(t, &n)?,
            None => std::ptr::null_mut(),
        };
        // SAFETY: the live receiver; the fields are resolved from its class.
        unsafe {
            obj::write::<i64>(this, uid_offset(), uid as i64);
            obj::write::<i64>(this, gid_offset(), gid as i64);
            obj::write::<Oop>(this, groups_offset(), arr);
            obj::write::<Oop>(this, username_offset(), name);
        }
        ret_void()
    });
}

fn supplementary_groups() -> Vec<libc::gid_t> {
    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut groups = vec![0; n.max(0) as usize];
    let got = unsafe { libc::getgroups(n, groups.as_mut_ptr()) };
    groups.truncate(got.max(0) as usize);
    groups
}

fn user_name(uid: libc::uid_t) -> Option<String> {
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut out = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r filled pw_name with a NUL-terminated string in `buf`.
    Some(unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }.to_string_lossy().into_owned())
}
