//! Reference statics as young-collection roots, without a write barrier. A static slot whose
//! value is the one it held when the last collection ended references an object that
//! collection kept (old now), or null: only slots whose value changed since can reference a
//! young object. The slots of every class are listed flat with the value each held then, and
//! a young collection visits only those that differ (HotSpot keeps statics in class mirrors
//! in the heap, where its card table finds the changed ones). Scanning every static of every
//! class through its field table was most of a young collection's fixed cost on a modded
//! server (tens of thousands of classes).

use crate::klass::Klass;
use std::sync::Mutex;

struct Slots {
    /// Addresses of the reference static slots of `classes` classes.
    addr: Vec<usize>,
    /// The value of each when the last collection ended.
    seen: Vec<usize>,
    classes: usize,
}

static SLOTS: Mutex<Slots> = Mutex::new(Slots { addr: Vec::new(), seen: Vec::new(), classes: 0 });

/// The listed slots, grown to the statics of `classes[..old]` (any not listed yet count as
/// changed), for a young collection: `(addresses, values then)`, cloned out of the lock.
pub(super) fn listed(classes: &[&'static Klass], old: usize) -> (Vec<usize>, Vec<usize>) {
    let mut s = SLOTS.lock().unwrap();
    extend(&mut s, classes, old, false);
    (s.addr.clone(), s.seen.clone())
}

/// After a collection: lists the statics of every class and records the values the slots
/// hold now; with `changed` (a young collection), only those slots and new classes' changed.
pub(super) fn settle(classes: &[&'static Klass], changed: Option<&[usize]>) {
    let mut s = SLOTS.lock().unwrap();
    match changed {
        Some(indices) => {
            for &i in indices {
                s.seen[i] = read(s.addr[i]);
            }
        }
        None => {
            for i in 0..s.addr.len() {
                s.seen[i] = read(s.addr[i]);
            }
        }
    }
    extend(&mut s, classes, classes.len(), true);
}

fn extend(s: &mut Slots, classes: &[&'static Klass], to: usize, current: bool) {
    if to <= s.classes {
        return;
    }
    crate::vm::vm().class_statics(&classes[s.classes..to], &mut |slot| {
        s.addr.push(slot);
        s.seen.push(if current { read(slot) } else { 0 });
    });
    s.classes = to;
}

/// The reference in a static slot (full width).
pub(super) fn read(slot: usize) -> usize {
    // SAFETY: a reference static's slot, read while the world is stopped.
    unsafe { *(slot as *const usize) }
}
