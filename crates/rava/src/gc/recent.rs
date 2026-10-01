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

/// Records `o`, just stored into a write-once VM root.
pub(crate) fn note(o: Oop) {
    if !o.is_null() {
        NOTED.lock().unwrap().push(o as usize);
    }
}

/// What a collection must still scan: the noted objects and the number of classes whose
/// write-once roots are already old. Starts the next period with `classes` defined.
pub(super) fn take(classes: usize) -> (Vec<usize>, usize) {
    let noted = std::mem::take(&mut *NOTED.lock().unwrap());
    (noted, CLASSES_SEEN.swap(classes, Ordering::Relaxed))
}
