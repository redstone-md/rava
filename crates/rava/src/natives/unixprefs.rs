//! FileSystemPreferences: Unix file modes and whole-file nonblocking record locks.

use super::{ret_int, ret_ref, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime;
use crate::types::BasicType;
use std::ffi::CString;

const CLASS: &str = "java/util/prefs/FileSystemPreferences";

fn path(o: u64) -> Result<CString, i32> {
    CString::new(runtime::java_string_to_rust(o as Oop)).map_err(|_| libc::EINVAL)
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(CLASS, "chmod", "(Ljava/lang/String;I)I", |t, a| {
        let result = match path(a[0]) {
            Ok(path) => crate::gc::safepoint::blocking(t, || crate::os::prefs_chmod(&path, a[1] as u32)),
            Err(error) => error,
        };
        ret_int(result)
    });
    r.add(CLASS, "lockFile0", "(Ljava/lang/String;IZ)[I", |t, a| {
        let result = match path(a[0]) {
            Ok(path) => crate::gc::safepoint::blocking(t, || crate::os::prefs_lock(&path, a[1] as u32, a[2] != 0)),
            Err(error) => (0, error),
        };
        let k = crate::vm::vm().primitive_array_class(BasicType::Int).expect("[I");
        let array = match runtime::alloc_array(t, k, 2) {
            Ok(array) => array,
            Err(error) => {
                if result.0 != 0 { crate::gc::safepoint::blocking(t, || crate::os::prefs_unlock(result.0)); }
                return Err(error);
            }
        };
        // SAFETY: a freshly allocated int[2].
        unsafe { obj::array_slice::<i32>(array).copy_from_slice(&[result.0, result.1]) };
        ret_ref(array)
    });
    r.add(CLASS, "unlockFile0", "(I)I", |t, a| {
        ret_int(crate::gc::safepoint::blocking(t, || crate::os::prefs_unlock(a[0] as i32)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_register_the_unix_preferences_backend() {
        let registry = NativeRegistry::with_builtins();
        for (name, sig) in [("chmod", "(Ljava/lang/String;I)I"), ("lockFile0", "(Ljava/lang/String;IZ)[I"), ("unlockFile0", "(I)I")] {
            assert!(registry.table.contains_key(&(CLASS, name, sig)));
        }
    }
}
