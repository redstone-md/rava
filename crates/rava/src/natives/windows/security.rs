//! `sun.nio.fs.WindowsNativeDispatcher`, the security side: security descriptors, ACLs, SIDs,
//! account lookups, access tokens and access checks, for file permissions and owners
//! (`AclFileAttributeView`, `Files.isWritable`).

use super::args::{check, last_error, string, string_at, Args};
use crate::heap::Oop;
use crate::natives::fields::{set_int, set_string};
use crate::natives::{ret_bool, ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_TOKEN, HANDLE, LUID};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken, SetThreadToken};

const D: &str = "sun/nio/fs/WindowsNativeDispatcher";
const ACCOUNT: &str = "sun/nio/fs/WindowsNativeDispatcher$Account";
const ACL_INFO: &str = "sun/nio/fs/WindowsNativeDispatcher$AclInformation";
const ACL_REVISION: u32 = 2;
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

/// A size query: succeeds, or fails only for want of room; either way the size needed.
fn needed(t: &mut crate::thread::Thread, ok: bool, size: u32) -> crate::thread::JResult<u64> {
    // SAFETY: a plain query.
    if ok || unsafe { GetLastError() } == ERROR_INSUFFICIENT_BUFFER {
        return ret_int(size as i32);
    }
    Err(last_error(t))
}

fn descriptors(r: &mut NativeRegistry) {
    r.add(D, "GetFileSecurity0", "(JIJI)I", |t, a| {
        let mut a = Args::new(a);
        let (path, info, desc, len) = (a.ptr::<u16>(), a.int() as u32, a.ptr::<std::ffi::c_void>(), a.int() as u32);
        let mut n = 0u32;
        // SAFETY: a path and the caller's buffer of `len` bytes.
        let ok = unsafe { GetFileSecurityW(path, info, desc, len, &mut n) } != 0;
        needed(t, ok, n)
    });
    r.add(D, "SetFileSecurity0", "(JIJ)V", |t, a| {
        let mut a = Args::new(a);
        let (path, info, desc) = (a.ptr::<u16>(), a.int() as u32, a.ptr::<std::ffi::c_void>());
        // SAFETY: a path and a security descriptor.
        check(t, unsafe { SetFileSecurityW(path, info, desc) } != 0)?;
        ret_void()
    });
    r.add(D, "GetSecurityDescriptorOwner", "(J)J", |t, a| {
        let (mut owner, mut defaulted) = (std::ptr::null_mut(), 0);
        // SAFETY: a security descriptor.
        check(t, unsafe { GetSecurityDescriptorOwner(a[0] as PSECURITY_DESCRIPTOR, &mut owner, &mut defaulted) } != 0)?;
        ret_long(owner as i64)
    });
    r.add(D, "SetSecurityDescriptorOwner", "(JJ)V", |t, a| {
        // SAFETY: a descriptor and a SID.
        check(t, unsafe { SetSecurityDescriptorOwner(a[0] as PSECURITY_DESCRIPTOR, a[2] as PSID, 0) } != 0)?;
        ret_void()
    });
    r.add(D, "GetSecurityDescriptorDacl", "(J)J", |t, a| {
        let (mut present, mut acl, mut defaulted) = (0, std::ptr::null_mut(), 0);
        // SAFETY: a security descriptor.
        check(t, unsafe { GetSecurityDescriptorDacl(a[0] as PSECURITY_DESCRIPTOR, &mut present, &mut acl, &mut defaulted) } != 0)?;
        ret_long(if present != 0 { acl as i64 } else { 0 })
    });
    r.add(D, "SetSecurityDescriptorDacl", "(JJ)V", |t, a| {
        // SAFETY: a descriptor and an ACL.
        check(t, unsafe { SetSecurityDescriptorDacl(a[0] as PSECURITY_DESCRIPTOR, 1, a[2] as *const ACL, 0) } != 0)?;
        ret_void()
    });
    r.add(D, "InitializeSecurityDescriptor", "(J)V", |t, a| {
        // SAFETY: the caller's descriptor buffer.
        check(t, unsafe { InitializeSecurityDescriptor(a[0] as PSECURITY_DESCRIPTOR, SECURITY_DESCRIPTOR_REVISION) } != 0)?;
        ret_void()
    });
    r.add(D, "InitializeAcl", "(JI)V", |t, a| {
        // SAFETY: the caller's ACL buffer of that size.
        check(t, unsafe { InitializeAcl(a[0] as *mut ACL, a[2] as u32, ACL_REVISION) } != 0)?;
        ret_void()
    });
    r.add(D, "GetAclInformation0", "(JLsun/nio/fs/WindowsNativeDispatcher$AclInformation;)V", |t, a| {
        // SAFETY: an ACL and a size-information struct.
        let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetAclInformation(a[0] as *const ACL, &mut info as *mut _ as *mut _, std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation) } != 0;
        check(t, ok)?;
        set_int(a[2] as Oop, ACL_INFO, "aceCount", info.AceCount as i32);
        ret_void()
    });
    r.add(D, "GetAce", "(JI)J", |t, a| {
        let mut ace = std::ptr::null_mut();
        // SAFETY: an ACL.
        check(t, unsafe { GetAce(a[0] as *const ACL, a[2] as u32, &mut ace) } != 0)?;
        ret_long(ace as i64)
    });
    for (name, allowed) in [("AddAccessAllowedAceEx", true), ("AddAccessDeniedAceEx", false)] {
        let f: crate::natives::NativeFn = if allowed {
            |t, a| {
                let mut a = Args::new(a);
                let (acl, flags, mask, sid) = (a.ptr::<ACL>(), a.int() as u32, a.int() as u32, a.ptr::<std::ffi::c_void>());
                // SAFETY: an ACL with room and a SID.
                check(t, unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, flags as ACE_FLAGS, mask, sid) } != 0)?;
                ret_void()
            }
        } else {
            |t, a| {
                let mut a = Args::new(a);
                let (acl, flags, mask, sid) = (a.ptr::<ACL>(), a.int() as u32, a.int() as u32, a.ptr::<std::ffi::c_void>());
                // SAFETY: an ACL with room and a SID.
                check(t, unsafe { AddAccessDeniedAceEx(acl, ACL_REVISION, flags as ACE_FLAGS, mask, sid) } != 0)?;
                ret_void()
            }
        };
        r.add(D, name, "(JIIJ)V", f);
    }
}

fn accounts(r: &mut NativeRegistry) {
    r.add(D, "LookupAccountSid0", "(JLsun/nio/fs/WindowsNativeDispatcher$Account;)V", |t, a| {
        let (mut name, mut domain) = ([0u16; 256], [0u16; 256]);
        let (mut nl, mut dl, mut usage) = (256u32, 256u32, 0);
        // SAFETY: a SID and buffers of the lengths passed.
        let ok = unsafe { LookupAccountSidW(std::ptr::null(), a[0] as PSID, name.as_mut_ptr(), &mut nl, domain.as_mut_ptr(), &mut dl, &mut usage) } != 0;
        check(t, ok)?;
        let o = a[2] as Oop;
        let d = string(t, &domain)?;
        set_string(o, ACCOUNT, "domain", d);
        let n = string(t, &name)?;
        set_string(o, ACCOUNT, "name", n);
        set_int(o, ACCOUNT, "use", usage);
        ret_void()
    });
    r.add(D, "LookupAccountName0", "(JJI)I", |t, a| {
        let mut a = Args::new(a);
        let (name, sid, mut size) = (a.ptr::<u16>(), a.ptr::<std::ffi::c_void>(), a.int() as u32);
        let mut domain = [0u16; 256];
        let (mut dl, mut usage) = (256u32, 0);
        // SAFETY: a name and the caller's SID buffer of `size` bytes.
        let ok = unsafe { LookupAccountNameW(std::ptr::null(), name, sid, &mut size, domain.as_mut_ptr(), &mut dl, &mut usage) } != 0;
        needed(t, ok, size)
    });
    // SAFETY: a valid SID.
    r.add(D, "GetLengthSid", "(J)I", |_, a| ret_int(unsafe { GetLengthSid(a[0] as PSID) } as i32));
    r.add(D, "ConvertSidToStringSid", "(J)Ljava/lang/String;", |t, a| {
        let mut s = std::ptr::null_mut();
        // SAFETY: a SID; the string is freed after copying.
        check(t, unsafe { ConvertSidToStringSidW(a[0] as PSID, &mut s) } != 0)?;
        let r = string_at(t, s);
        unsafe { windows_sys::Win32::Foundation::LocalFree(s as *mut _) };
        ret_ref(r?)
    });
    r.add(D, "ConvertStringSidToSid0", "(J)J", |t, a| {
        let mut sid = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SID string; the class library frees the SID with LocalFree.
        check(t, unsafe { ConvertStringSidToSidW(a[0] as *const u16, &mut sid) } != 0)?;
        ret_long(sid as i64)
    });
}

fn tokens(r: &mut NativeRegistry) {
    // SAFETY: pseudo-handles of the caller.
    r.add(D, "GetCurrentProcess", "()J", |_, _| ret_long(unsafe { GetCurrentProcess() } as i64));
    r.add(D, "GetCurrentThread", "()J", |_, _| ret_long(unsafe { GetCurrentThread() } as i64));
    r.add(D, "OpenProcessToken", "(JI)J", |t, a| {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: a process handle.
        check(t, unsafe { OpenProcessToken(a[0] as HANDLE, a[2] as u32, &mut token) } != 0)?;
        ret_long(token as i64)
    });
    r.add(D, "OpenThreadToken", "(JIZ)J", |t, a| {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: a thread handle.
        if unsafe { OpenThreadToken(a[0] as HANDLE, a[2] as u32, a[3] as i32, &mut token) } == 0 {
            // SAFETY: a plain query.
            if unsafe { GetLastError() } == ERROR_NO_TOKEN {
                return ret_long(0);
            }
            return Err(last_error(t));
        }
        ret_long(token as i64)
    });
    r.add(D, "DuplicateTokenEx", "(JI)J", |t, a| {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: a token handle.
        let ok = unsafe { DuplicateTokenEx(a[0] as HANDLE, a[2] as u32, std::ptr::null(), SecurityImpersonation, TokenImpersonation, &mut token) } != 0;
        check(t, ok)?;
        ret_long(token as i64)
    });
    r.add(D, "SetThreadToken", "(JJ)V", |t, a| {
        let thread = a[0] as HANDLE;
        // SAFETY: a thread handle (by address, as the call takes it) and a token.
        check(t, unsafe { SetThreadToken(&thread, a[2] as HANDLE) } != 0)?;
        ret_void()
    });
    r.add(D, "GetTokenInformation", "(JIJI)I", |t, a| {
        let mut a = Args::new(a);
        let (token, class, buf, len) = (a.long() as HANDLE, a.int(), a.ptr::<std::ffi::c_void>(), a.int() as u32);
        let mut n = 0u32;
        // SAFETY: a token and the caller's buffer of `len` bytes.
        let ok = unsafe { GetTokenInformation(token, class as TOKEN_INFORMATION_CLASS, buf, len, &mut n) } != 0;
        needed(t, ok, n)
    });
    r.add(D, "LookupPrivilegeValue0", "(J)J", |t, a| {
        // SAFETY: a LUID the class library frees; a NUL-terminated privilege name.
        let luid = unsafe { windows_sys::Win32::System::Memory::LocalAlloc(0, std::mem::size_of::<LUID>()) } as *mut LUID;
        if luid.is_null() || unsafe { LookupPrivilegeValueW(std::ptr::null(), a[0] as *const u16, luid) } == 0 {
            return Err(last_error(t));
        }
        ret_long(luid as i64)
    });
    r.add(D, "AdjustTokenPrivileges", "(JJI)V", |t, a| {
        let mut a = Args::new(a);
        let (token, luid, attrs) = (a.long() as HANDLE, a.ptr::<LUID>(), a.int() as u32);
        // SAFETY: a token and a LUID from LookupPrivilegeValue0.
        let ok = unsafe {
            let tp = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: *luid, Attributes: attrs }] };
            AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut()) != 0
        };
        check(t, ok)?;
        ret_void()
    });
    r.add(D, "AccessCheck", "(JJIIIII)Z", |t, a| {
        let mut a = Args::new(a);
        let (token, desc) = (a.long() as HANDLE, a.ptr::<std::ffi::c_void>());
        let mut mask = a.int() as u32;
        let mapping = GENERIC_MAPPING { GenericRead: a.int() as u32, GenericWrite: a.int() as u32, GenericExecute: a.int() as u32, GenericAll: a.int() as u32 };
        let mut privileges = [0u8; 256];
        let (mut plen, mut granted, mut status) = (privileges.len() as u32, 0u32, 0);
        // SAFETY: a token, a security descriptor and buffers of the sizes passed.
        let ok = unsafe {
            MapGenericMask(&mut mask, &mapping);
            AccessCheck(desc, token, mask, &mapping, privileges.as_mut_ptr() as *mut _, &mut plen, &mut granted, &mut status) != 0
        };
        check(t, ok)?;
        ret_bool(status != 0)
    });
}

pub(super) fn register(r: &mut NativeRegistry) {
    descriptors(r);
    accounts(r);
    tokens(r);
}
