//! The macOS java.prefs natives: genuine CFPreferences storage and the JDK's domain handles.

use super::{ret_bool, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::os::{MacPrefs, MacPrefsResult};
use crate::runtime as rt;
use crate::thread::{JResult, Thread};

const CLASS: &str = "java/util/prefs/MacOSXPreferencesFile";

/// Copy every Java string before CF may block. Only owned Rust buffers and CF objects are
/// touched outside Java; result strings/arrays are allocated after the thread resumes.
fn call<T>(t: &mut Thread, args: &[u64], strings: usize, f: impl FnOnce(&MacPrefs, &[Vec<u16>]) -> MacPrefsResult<T>) -> JResult<Option<T>> {
    let mut inputs = Vec::with_capacity(strings);
    for &value in &args[..strings] {
        if value == 0 { return Ok(None) }
        inputs.push(rt::string_units(value as Oop));
    }
    let name = inputs.pop().expect("preferences application argument");
    // Longs each occupy two Java frame slots.
    let (user, host) = (args[strings], args[strings + 2]);
    let result = crate::gc::safepoint::blocking(t, || {
        let domain = MacPrefs::new(name, user, host)?;
        f(&domain, &inputs)
    });
    result.map(Some).map_err(|_| rt::throw_new(t, "java/lang/OutOfMemoryError", "CoreFoundation preferences allocation"))
}

fn names(t: &mut Thread, args: &[u64], children: bool) -> JResult<u64> {
    let Some(names) = call(t, args, 2, |domain, strings| domain.names(&strings[0], children))? else {
        return ret_ref(std::ptr::null_mut());
    };
    let string = crate::loader::boot_class!("java/lang/String").expect("String");
    let array = rt::alloc_array(t, crate::vm::vm().array_class_of(string).expect("String[]"), names.len() as i32)?;
    // Pin the array explicitly while allocations make each element. Rust containers contain
    // only UTF-16 buffers, and each returned Java String is stored before another allocation.
    let roots = t.handles.len();
    t.handles.push(array);
    let result = (|| {
        for (i, units) in names.iter().enumerate() {
            let value = rt::new_string_utf16(t, units)?;
            // SAFETY: a rooted String[] of names.len() elements.
            unsafe { obj::store_elem(array, i, value) };
        }
        ret_ref(array)
    })();
    t.handles.truncate(roots);
    result
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(CLASS, "currentUser", "()J", |_, _| ret_long(crate::os::mac_prefs_constants()[0] as i64));
    r.add(CLASS, "anyUser", "()J", |_, _| ret_long(crate::os::mac_prefs_constants()[1] as i64));
    r.add(CLASS, "currentHost", "()J", |_, _| ret_long(crate::os::mac_prefs_constants()[2] as i64));
    r.add(CLASS, "anyHost", "()J", |_, _| ret_long(crate::os::mac_prefs_constants()[3] as i64));
    // Descriptor literals follow MacOSXPreferencesFile in the macOS JDK, not Unix's backend.
    r.add(CLASS, "addNode", "(Ljava/lang/String;Ljava/lang/String;JJ)Z", |t, a| {
        ret_bool(call(t, a, 2, |d, s| d.add_node(&s[0]))?.unwrap_or(false))
    });
    r.add(CLASS, "removeNode", "(Ljava/lang/String;Ljava/lang/String;JJ)V", |t, a| {
        call(t, a, 2, |d, s| d.remove_node(&s[0]))?;
        ret_void()
    });
    r.add(CLASS, "addChildToNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)Z", |t, a| {
        ret_bool(call(t, a, 3, |d, s| d.add_child(&s[0], &s[1]))?.unwrap_or(false))
    });
    r.add(CLASS, "removeChildFromNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)V", |t, a| {
        call(t, a, 3, |d, s| d.remove(&s[0], &s[1]))?;
        ret_void()
    });
    r.add(CLASS, "addKeyToNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)V", |t, a| {
        call(t, a, 4, |d, s| d.add_key(&s[0], &s[1], &s[2]))?;
        ret_void()
    });
    r.add(CLASS, "removeKeyFromNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)V", |t, a| {
        call(t, a, 3, |d, s| d.remove(&s[0], &s[1]))?;
        ret_void()
    });
    r.add(CLASS, "getKeyFromNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)Ljava/lang/String;", |t, a| {
        let value = match call(t, a, 3, |d, s| d.get_key(&s[0], &s[1]))?.flatten() {
            Some(units) => rt::new_string_utf16(t, &units)?,
            None => std::ptr::null_mut(),
        };
        ret_ref(value)
    });
    r.add(CLASS, "getChildrenForNode", "(Ljava/lang/String;Ljava/lang/String;JJ)[Ljava/lang/String;", |t, a| names(t, a, true));
    r.add(CLASS, "getKeysForNode", "(Ljava/lang/String;Ljava/lang/String;JJ)[Ljava/lang/String;", |t, a| names(t, a, false));
    r.add(CLASS, "synchronize", "(Ljava/lang/String;JJ)Z", |t, a| {
        ret_bool(call(t, a, 1, |d, _| Ok(d.synchronize()))?.unwrap_or(false))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_every_macos_preferences_native() {
        let registry = NativeRegistry::with_builtins();
        for (name, signature) in [
            ("currentUser", "()J"), ("anyUser", "()J"), ("currentHost", "()J"), ("anyHost", "()J"),
            ("addNode", "(Ljava/lang/String;Ljava/lang/String;JJ)Z"), ("removeNode", "(Ljava/lang/String;Ljava/lang/String;JJ)V"),
            ("addChildToNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)Z"), ("removeChildFromNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)V"),
            ("addKeyToNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)V"), ("removeKeyFromNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)V"),
            ("getKeyFromNode", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJ)Ljava/lang/String;"), ("getChildrenForNode", "(Ljava/lang/String;Ljava/lang/String;JJ)[Ljava/lang/String;"),
            ("getKeysForNode", "(Ljava/lang/String;Ljava/lang/String;JJ)[Ljava/lang/String;"), ("synchronize", "(Ljava/lang/String;JJ)Z"),
        ] {
            assert!(registry.table.contains_key(&(CLASS, name, signature)));
        }
    }
}
