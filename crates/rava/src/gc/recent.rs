//! Roots created since the last collection, so a young collection need not rescan every class.
//!
//! Class roots other than statics only ever go from null to an object: a mirror, a resolved
//! constant, an interned string. Once a collection has marked such an object it is old, and a
//! young collection never needs to reach it again. So the VM notes each new one here, and a
//! young collection scans the noted objects, the statics of every class, and the full roots of
//! the classes defined since the last collection only.

use crate::heap::Oop;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Objects stored into write-once VM roots since the last collection.
static NOTED: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Classes defined (in `all_classes` order) when the last collection ran.
static CLASSES_SEEN: AtomicUsize = AtomicUsize::new(0);

/// Objects compiled code holds as constants (and interned VM objects): roots of every full
/// collection, so they never move. A young collection marks only those embedded since the last
/// collection (also noted): the others are old already, and an old root is never young again.
static EMBEDDED: Mutex<Option<std::collections::HashSet<usize>>> = Mutex::new(None);

/// Records `o` as a constant of compiled code (kept in place for good).
pub(crate) fn embed(o: Oop) {
    if !o.is_null() && EMBEDDED.lock().unwrap().get_or_insert_with(Default::default).insert(o as usize) {
        note(o);
    }
}

/// Records `o`, just stored into a write-once VM root.
pub(crate) fn note(o: Oop) {
    if !o.is_null() {
        NOTED.lock().unwrap().push(o as usize);
    }
}

/// What a collection must still scan: the noted objects (and, for a full one, every embedded
/// object) and the number of classes whose write-once roots are already old. Starts the next
/// period with `classes` defined.
pub(super) fn take(classes: usize, young: bool) -> (Vec<usize>, usize) {
    let mut noted = std::mem::take(&mut *NOTED.lock().unwrap());
    if !young {
        noted.extend(EMBEDDED.lock().unwrap().iter().flatten());
    }
    (noted, CLASSES_SEEN.swap(classes, Ordering::Relaxed))
}

/// Every object compiled code holds as a constant (`why.rs`).
pub(super) fn embedded() -> Vec<usize> {
    EMBEDDED.lock().unwrap().iter().flatten().copied().collect()
}
