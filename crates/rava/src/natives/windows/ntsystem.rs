//! `com.sun.security.auth.module.NTSystem`, the current Windows user as the JDK's JAAS natives
//! read it from the process token: name, domain, the user's, domain's, primary group's and
//! every group's SID, and an impersonation token. Servers check the groups for the
//! Administrators SID to warn about running elevated.

use super::args::string;
use crate::heap::{obj, Oop};
use crate::natives::{ret_long, ret_void, NativeRegistry};
use crate::runtime as rt;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::vm::vm;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    DuplicateToken, GetTokenInformation, LookupAccountSidW, SecurityImpersonation, TokenGroups, TokenPrimaryGroup, TokenUser, PSID, SID_NAME_USE, TOKEN_DUPLICATE, TOKEN_GROUPS,
    TOKEN_INFORMATION_CLASS, TOKEN_PRIMARY_GROUP, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const C: &str = "com/sun/security/auth/module/NTSystem";

/// Writes field `field` (of descriptor `desc`) of `o`, found in `o`'s own class.
fn put(o: Oop, field: &str, desc: &str, write: impl FnOnce(u32)) {
    // SAFETY: a live NTSystem.
    let k = unsafe { obj::klass(o) };
    if let Some(f) = k.declared_field(Symbol::intern(field), Symbol::intern(desc)) {
        write(f.offset);
    }
}

fn set_string(o: Oop, field: &str, v: Oop) {
    // SAFETY: a reference field of the live object.
    put(o, field, if field == "groupIDs" { "[Ljava/lang/String;" } else { "Ljava/lang/String;" }, |at| unsafe { obj::write_ref(o, at, v) });
}

/// The process token, closed when dropped.
struct Token(HANDLE);

impl Token {
    fn open(access: u32) -> Option<Token> {
        let mut h: HANDLE = std::ptr::null_mut();
        // SAFETY: opens this process's own token.
        (unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut h) } != 0).then_some(Token(h))
    }

    /// A token information class into an 8-aligned buffer.
    fn info(&self, class: TOKEN_INFORMATION_CLASS) -> Option<Vec<u64>> {
        let mut n = 0u32;
        // SAFETY: a size query, then the read into a buffer of that size.
        unsafe {
            GetTokenInformation(self.0, class, std::ptr::null_mut(), 0, &mut n);
            let mut buf = vec![0u64; (n as usize).div_ceil(8)];
            (GetTokenInformation(self.0, class, buf.as_mut_ptr() as *mut _, n, &mut n) != 0).then_some(buf)
        }
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: the handle `open` got.
        unsafe { CloseHandle(self.0) };
    }
}

/// A SID in its string form (`S-1-5-...`).
fn sid_string(sid: PSID) -> Option<String> {
    let mut p: *mut u16 = std::ptr::null_mut();
    // SAFETY: a valid SID; the string is freed with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut p) == 0 {
            return None;
        }
        let n = (0..).take_while(|&i| *p.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        LocalFree(p as _);
        Some(s)
    }
}

/// The account name and domain of a SID.
fn account(sid: PSID) -> Option<(Vec<u16>, Vec<u16>)> {
    let (mut name, mut domain) = (vec![0u16; 256], vec![0u16; 256]);
    let (mut nl, mut dl, mut usage) = (256u32, 256u32, 0 as SID_NAME_USE);
    // SAFETY: buffers of the lengths passed.
    let ok = unsafe { LookupAccountSidW(std::ptr::null(), sid, name.as_mut_ptr(), &mut nl, domain.as_mut_ptr(), &mut dl, &mut usage) } != 0;
    ok.then(|| (name[..nl as usize].to_vec(), domain[..dl as usize].to_vec()))
}

fn set_text(t: &mut Thread, o: Oop, field: &str, v: Option<String>) -> JResult<()> {
    let s = match v {
        Some(v) => rt::new_string(t, &v)?,
        None => std::ptr::null_mut(),
    };
    set_string(o, field, s);
    Ok(())
}

/// `getCurrent(debug)`: fills the fields from the process token.
fn current(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let me = a[0] as Oop;
    let Some(token) = Token::open(TOKEN_QUERY) else { return ret_void() };
    if let Some(buf) = token.info(TokenUser) {
        // SAFETY: a TOKEN_USER as GetTokenInformation laid it out.
        let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
        let user = sid_string(sid);
        if let Some((name, domain)) = account(sid) {
            let name = string(t, &name)?;
            set_string(me, "userName", name);
            let domain = string(t, &domain)?;
            set_string(me, "domain", domain);
        }
        // The domain's SID is the user's without its last (relative) part.
        set_text(t, me, "domainSID", user.as_ref().and_then(|u| u.rsplit_once('-')).map(|(d, _)| d.to_string()))?;
        set_text(t, me, "userSID", user)?;
    }
    let primary = token.info(TokenPrimaryGroup).and_then(|buf| {
        // SAFETY: a TOKEN_PRIMARY_GROUP.
        sid_string(unsafe { (*(buf.as_ptr() as *const TOKEN_PRIMARY_GROUP)).PrimaryGroup })
    });
    if let Some(buf) = token.info(TokenGroups) {
        // SAFETY: a TOKEN_GROUPS with GroupCount entries. The primary group is listed apart, as
        // the JDK lists it.
        let groups: Vec<String> = unsafe {
            let g = &*(buf.as_ptr() as *const TOKEN_GROUPS);
            std::slice::from_raw_parts(g.Groups.as_ptr(), g.GroupCount as usize).iter().filter_map(|e| sid_string(e.Sid)).filter(|s| Some(s) != primary.as_ref()).collect()
        };
        let string_class = crate::loader::boot_class!("java/lang/String").expect("String");
        let arr = rt::alloc_array(t, vm().array_class_of(string_class).expect("String[]"), groups.len() as i32)?;
        for (i, g) in groups.iter().enumerate() {
            let s = rt::new_string(t, g)?;
            // SAFETY: a String[] of groups.len() elements.
            unsafe { obj::store_elem(arr, i, s) };
        }
        set_string(me, "groupIDs", arr);
    }
    set_text(t, me, "primaryGroupID", primary)?;
    let token = impersonation_token();
    // SAFETY: a long field of the live object.
    put(me, "impersonationToken", "J", |at| unsafe { obj::write::<i64>(me, at, token) });
    ret_void()
}

/// A duplicate of the process token for impersonation, as a handle (0 when there is none).
fn impersonation_token() -> i64 {
    let Some(token) = Token::open(TOKEN_DUPLICATE) else { return 0 };
    let mut dup: HANDLE = std::ptr::null_mut();
    // SAFETY: duplicates an open token; the caller owns the copy.
    if unsafe { DuplicateToken(token.0, SecurityImpersonation, &mut dup) } == 0 {
        return 0;
    }
    dup as i64
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(C, "getCurrent", "(Z)V", current);
    r.add(C, "getImpersonationToken0", "()J", |_, _| ret_long(impersonation_token()));
}
