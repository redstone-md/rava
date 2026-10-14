//! Small natives of the Windows class library: the error mode, the environment block, the
//! system's random seed, the NTLM trusted-site check.

use crate::heap::{obj, Oop};
use crate::natives::{ret_bool, ret_long, ret_ref, NativeRegistry};
use crate::runtime as rt;
use windows_sys::Win32::System::Diagnostics::Debug::{SetErrorMode, THREAD_ERROR_MODE};

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("sun/io/Win32ErrorMode", "setErrorMode", "(J)J", |_, a| {
        // SAFETY: a plain call.
        ret_long(unsafe { SetErrorMode(a[0] as THREAD_ERROR_MODE) } as i64)
    });
    // `ProcessEnvironment.environmentBlock`: `NAME=value` entries, each ended by a NUL.
    r.add("java/lang/ProcessEnvironment", "environmentBlock", "()Ljava/lang/String;", |t, _| {
        let mut block = String::new();
        for (k, v) in std::env::vars_os() {
            block.push_str(&k.to_string_lossy());
            block.push('=');
            block.push_str(&v.to_string_lossy());
            block.push('\0');
        }
        ret_ref(rt::new_string(t, &block)?)
    });
    // Fills the array from the system's random number generator.
    r.add("sun/security/provider/NativeSeedGenerator", "nativeGenerateSeed", "([B)Z", |_, a| {
        use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
        // SAFETY: a live byte[] filled in place.
        let buf = unsafe { obj::array_slice::<u8>(a[0] as Oop) };
        // SAFETY: the system RNG writes `len` bytes.
        let ok = unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) } == 0;
        ret_bool(ok)
    });
    // The JDK asks urlmon's zone map whether a site is in the intranet zone, for transparent
    // NTLM logins. Without that map the class takes its documented fallback: no site is
    // trusted, so NTLM credentials are only sent where an `Authenticator` supplies them.
    // `<clinit>` calls this on any `WWW-Authenticate` header (an HTTP 401 from anywhere).
    const NTLM: &str = "sun/net/www/protocol/http/ntlm/NTLMAuthentication";
    r.add(NTLM, "isTrustedSiteAvailable", "()Z", |_, _| ret_bool(false));
    r.add(NTLM, "isTrustedSite0", "(Ljava/lang/String;)Z", |_, _| ret_bool(false));
}
