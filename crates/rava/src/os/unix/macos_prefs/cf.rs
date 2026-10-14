//! Owned CoreFoundation objects used by the native preferences backend.

use std::ffi::c_void;
use std::ptr::{addr_of, null, NonNull};

pub(super) type Ref = *const c_void;
pub(super) type Result<T> = std::result::Result<T, ()>;

#[repr(C)]
struct Range { location: isize, length: isize }

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRetain(value: Ref) -> Ref;
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringCreateWithCharacters(allocator: Ref, chars: *const u16, count: isize) -> Ref;
    fn CFStringGetLength(value: Ref) -> isize;
    fn CFStringGetCharacters(value: Ref, range: Range, buffer: *mut u16);
    fn CFStringCreateWithFormat(allocator: Ref, options: Ref, format: Ref, ...) -> Ref;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: Ref) -> u8;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberIsFloatType(value: Ref) -> u8;
    fn CFNumberGetValue(value: Ref, kind: isize, buffer: *mut c_void) -> u8;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryCreateMutable(allocator: Ref, capacity: isize, keys: Ref, values: Ref) -> Ref;
    fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
    fn CFDictionaryContainsKey(dictionary: Ref, key: Ref) -> u8;
    fn CFDictionaryAddValue(dictionary: Ref, key: Ref, value: Ref);
    fn CFDictionarySetValue(dictionary: Ref, key: Ref, value: Ref);
    fn CFDictionaryRemoveValue(dictionary: Ref, key: Ref);
    fn CFDictionaryGetCount(dictionary: Ref) -> isize;
    fn CFDictionaryGetKeysAndValues(dictionary: Ref, keys: *mut Ref, values: *mut Ref);
    fn CFPropertyListCreateDeepCopy(allocator: Ref, value: Ref, options: usize) -> Ref;
    fn CFPreferencesCopyValue(key: Ref, application: Ref, user: Ref, host: Ref) -> Ref;
    fn CFPreferencesSetValue(key: Ref, value: Ref, application: Ref, user: Ref, host: Ref);
    fn CFPreferencesSynchronize(application: Ref, user: Ref, host: Ref) -> u8;
    // Only their addresses are passed to CF; these opaque callback structures are never read.
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFPreferencesCurrentUser: Ref;
    static kCFPreferencesAnyUser: Ref;
    static kCFPreferencesCurrentHost: Ref;
    static kCFPreferencesAnyHost: Ref;
}

pub(super) struct Owned(NonNull<c_void>);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: this object owns one Create/Copy/Retain reference.
        unsafe { CFRelease(self.raw()) };
    }
}

impl Owned {
    fn take(value: Ref) -> Result<Self> { NonNull::new(value as *mut c_void).map(Owned).ok_or(()) }
    pub(super) fn raw(&self) -> Ref { self.0.as_ptr() }

    pub(super) fn retained(&self) -> Self {
        // SAFETY: retaining a live object cannot fail and transfers one reference to the copy.
        Self::take(unsafe { CFRetain(self.raw()) }).expect("CF retained a live object")
    }

    pub(super) fn string(units: &[u16]) -> Result<Self> {
        // SAFETY: the input is an owned UTF-16 buffer, copied by CF.
        Self::take(unsafe { CFStringCreateWithCharacters(null(), units.as_ptr(), units.len() as isize) })
    }

    pub(super) fn dictionary() -> Result<Self> {
        // SAFETY: CFType callbacks retain/release keys and values in the returned dictionary.
        Self::take(unsafe { CFDictionaryCreateMutable(null(), 0, addr_of!(kCFTypeDictionaryKeyCallBacks).cast(), addr_of!(kCFTypeDictionaryValueCallBacks).cast()) })
    }

    pub(super) fn is_dictionary(&self) -> bool {
        // SAFETY: a live CF object.
        unsafe { CFGetTypeID(self.raw()) == CFDictionaryGetTypeID() }
    }

    pub(super) fn mutable_copy(&self) -> Result<Self> {
        // SAFETY: a property-list dictionary; option 1 makes all its containers mutable.
        Self::take(unsafe { CFPropertyListCreateDeepCopy(null(), self.raw(), 1) })
    }

    pub(super) fn get(&self, key: &Owned) -> Option<Self> {
        // SAFETY: a live dictionary/key. Retain the borrowed value before its parent can drop.
        let value = unsafe { CFDictionaryGetValue(self.raw(), key.raw()) };
        (!value.is_null()).then(|| Self::take(unsafe { CFRetain(value) }).expect("CF retained a live object"))
    }

    pub(super) fn contains(&self, key: &Owned) -> bool {
        // SAFETY: a live dictionary and key.
        unsafe { CFDictionaryContainsKey(self.raw(), key.raw()) != 0 }
    }

    pub(super) fn add(&self, key: &Owned, value: &Owned) {
        // SAFETY: this is a newly created/deep-copied mutable dictionary; CF retains the value.
        unsafe { CFDictionaryAddValue(self.raw(), key.raw(), value.raw()) };
    }

    pub(super) fn set(&self, key: &Owned, value: &Owned) {
        // SAFETY: as for add; replacement also retains the new value.
        unsafe { CFDictionarySetValue(self.raw(), key.raw(), value.raw()) };
    }

    pub(super) fn remove(&self, key: &Owned) {
        // SAFETY: a mutable dictionary and key.
        unsafe { CFDictionaryRemoveValue(self.raw(), key.raw()) };
    }

    pub(super) fn keys(&self) -> Vec<Self> {
        // SAFETY: the owned dictionary stays live and unchanged during enumeration.
        let count = unsafe { CFDictionaryGetCount(self.raw()) } as usize;
        let mut keys = vec![null(); count];
        unsafe { CFDictionaryGetKeysAndValues(self.raw(), keys.as_mut_ptr(), std::ptr::null_mut()) };
        keys.into_iter().map(|key| Self::take(unsafe { CFRetain(key) }).expect("CF dictionary key")).collect()
    }

    /// JDK preferences also accept numeric and boolean values written by other CF clients.
    pub(super) fn units(&self) -> Result<Option<Vec<u16>>> {
        // SAFETY: each type-specific operation below is guarded by the CF object's type ID.
        unsafe {
            let kind = CFGetTypeID(self.raw());
            if kind == CFStringGetTypeID() {
                let count = CFStringGetLength(self.raw());
                let mut units = vec![0u16; count as usize];
                if count != 0 { CFStringGetCharacters(self.raw(), Range { location: 0, length: count }, units.as_mut_ptr()); }
                return Ok(Some(units));
            }
            if kind == CFBooleanGetTypeID() {
                return Ok(Some(if CFBooleanGetValue(self.raw()) != 0 { "true" } else { "false" }.encode_utf16().collect()));
            }
            if kind == CFNumberGetTypeID() {
                let formatted = if CFNumberIsFloatType(self.raw()) != 0 {
                    let mut value = 0f64;
                    CFNumberGetValue(self.raw(), 13, (&mut value as *mut f64).cast());
                    let format = Self::string(&"%g".encode_utf16().collect::<Vec<_>>())?;
                    Self::take(CFStringCreateWithFormat(null(), null(), format.raw(), value))?
                } else {
                    let mut value = 0isize;
                    CFNumberGetValue(self.raw(), 10, (&mut value as *mut isize).cast());
                    let format = Self::string(&"%ld".encode_utf16().collect::<Vec<_>>())?;
                    Self::take(CFStringCreateWithFormat(null(), null(), format.raw(), value))?
                };
                return formatted.units();
            }
        }
        Ok(None)
    }
}

pub(super) fn constants() -> [u64; 4] {
    // SAFETY: immortal CoreFoundation preference domain constants, returned borrowed.
    unsafe { [kCFPreferencesCurrentUser as u64, kCFPreferencesAnyUser as u64, kCFPreferencesCurrentHost as u64, kCFPreferencesAnyHost as u64] }
}

pub(super) fn copy(key: &Owned, app: &Owned, user: u64, host: u64) -> Option<Owned> {
    // SAFETY: live CF strings; user and host are the CF constants supplied to the class library.
    Owned::take(unsafe { CFPreferencesCopyValue(key.raw(), app.raw(), user as Ref, host as Ref) }).ok()
}

pub(super) fn store(key: &Owned, value: Option<&Owned>, app: &Owned, user: u64, host: u64) {
    // SAFETY: preference values are property-list dictionaries, copied by CF; null deletes a key.
    unsafe { CFPreferencesSetValue(key.raw(), value.map_or(null(), Owned::raw), app.raw(), user as Ref, host as Ref) };
}

pub(super) fn synchronize(app: &Owned, user: u64, host: u64) -> bool {
    // SAFETY: a live CF application string and the preference user/host constants.
    unsafe { CFPreferencesSynchronize(app.raw(), user as Ref, host as Ref) != 0 }
}
