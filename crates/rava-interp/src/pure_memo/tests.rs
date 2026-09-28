//! Pure-memo tests.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::pure_memo::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_share_one_window_and_outer_close_clears() {
        let outer = Scope::enter(1, &[10], true);
        outer.store(42);
        {
            let inner = Scope::enter(1, &[10], true);
            assert_eq!(inner.lookup(), Some(42));
        }
        assert!(in_window());
        drop(outer);
        assert!(!in_window());

        let next = Scope::enter(1, &[10], true);
        assert_eq!(next.lookup(), None, "a new window must not see the old one's entries");
    }

    #[test]
    fn distinct_cells_are_distinct_keys() {
        let w = Scope::enter(7, &[1, 2], true);
        w.store(100);
        let other = Scope::enter(7, &[1, 3], true);
        assert_eq!(other.lookup(), None);
        drop(other);
        let same = Scope::enter(7, &[1, 2], true);
        assert_eq!(same.lookup(), Some(100));
    }

    #[test]
    fn taint_suppresses_stores_for_the_rest_of_the_window() {
        let outer = Scope::enter(3, &[5], true);
        taint();
        outer.store(9);
        let probe = Scope::enter(3, &[5], true);
        assert_eq!(probe.lookup(), None);
    }

    #[test]
    fn slot_shape_marks_references_and_widths() {
        // (DensityFunction$FunctionContext)D on an instance: recv + 1 ref arg.
        let (mask, slots) = desc_slot_shape("(Lfoo/Ctx;)D", false).unwrap();
        assert_eq!((mask, slots), (0b11, 2));
        // long + object: slots 1,2 are the long, slot 3 the ref.
        let (mask, slots) = desc_slot_shape("(JLfoo/A;)I", false).unwrap();
        assert_eq!((mask, slots), (0b1001, 4));
        // static with two ints
        let (mask, slots) = desc_slot_shape("(II)Z", true).unwrap();
        assert_eq!((mask, slots), (0b00, 2));
        // arrays are references
        let (mask, _) = desc_slot_shape("([D)D", true).unwrap();
        assert_eq!(mask, 0b1);
        // the widest shape the ring needs: noise(DDDDD)D = recv + 5 doubles = 11 slots
        let (mask, slots) = desc_slot_shape("(DDDDD)D", false).unwrap();
        assert_eq!((mask, slots), (0b1, 11));
        // wider than one key holds
        assert!(desc_slot_shape("(JJJJJJ)D", false).is_none());
    }

    #[test]
    fn only_primitive_returns_are_replayable() {
        assert_eq!(returns_primitive("(I)D"), Some(true));
        assert_eq!(returns_primitive("(I)J"), Some(true));
        assert_eq!(returns_primitive("(I)V"), Some(false));
        assert_eq!(returns_primitive("(I)Ljava/lang/Object;"), Some(false));
        assert_eq!(returns_primitive("(I)[I"), Some(false));
    }

    #[test]
    fn non_memoizable_scope_still_opens_a_window() {
        let outer = Scope::enter(4, &[1], false);
        assert!(in_window());
        outer.store(1); // no key ⇒ no-op
        let inner = Scope::enter(4, &[1], true);
        inner.store(77);
        drop(inner);
        let again = Scope::enter(4, &[1], true);
        assert_eq!(again.lookup(), Some(77), "inner entries live for the whole outer window");
    }
}
