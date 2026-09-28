//! Immutable class-load facts for payload stored in native synthetic slots.
//!
//! This is deliberately a lower-bound catalog: only exact provider tables may
//! prove a slot. Missing providers remain `Unknown`; JVM subtype or name-shape
//! guesses never establish native storage ownership.

use crate::heap::intern::Syn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SyntheticSlots(u32);

impl SyntheticSlots {
    const fn slot(slot: Syn) -> Self {
        Self(1 << slot as u32)
    }

    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) const fn contains(self, slot: Syn) -> bool {
        self.0 & (1 << slot as u32) != 0
    }
}

/// Direct provider evidence attached when a classfile is loaded.
///
/// `Unknown` means the native-storage catalog does not yet cover this class;
/// it is not evidence that instances are safe for arena allocation or JIT
/// admission. Inherited state and per-method dispatch are separate facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeStateFact {
    Unknown,
    SyntheticSlots(SyntheticSlots),
}

impl NativeStateFact {
    /// Resolve direct storage only from exact provider registries.
    ///
    /// This is a load-time binding fact, not an eligibility policy. Callers must
    /// keep `Unknown` conservative and must not infer absence from an unlisted
    /// class until the provider catalog is complete.
    pub(crate) fn for_classfile_name(class: &str) -> Self {
        let facts = crate::owner_bits::facts(class);
        let mut slots = None;

        if facts & (crate::owner_bits::FACT_NATIVE_MAP | crate::owner_bits::FACT_FORCE_NATIVE_MAP)
            != 0
        {
            slots = Some(SyntheticSlots::slot(Syn::Map));
        }
        if facts & crate::owner_bits::FACT_ENTRY_TYPE != 0 {
            let entry = SyntheticSlots::slot(Syn::Key).union(SyntheticSlots::slot(Syn::Val));
            slots = Some(slots.map_or(entry, |known| known.union(entry)));
        }
        if facts & crate::owner_bits::FACT_NATIVE_REPR != 0 {
            let sequence = SyntheticSlots::slot(Syn::Elems);
            slots = Some(slots.map_or(sequence, |known| known.union(sequence)));
        }

        slots.map_or(Self::Unknown, Self::SyntheticSlots)
    }
}

impl NativeStateFact {
    pub(crate) const fn slots(self) -> Option<SyntheticSlots> {
        match self {
            Self::Unknown => None,
            Self::SyntheticSlots(slots) => Some(slots),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::{NativeStateFact, Syn};

    #[test]
    fn exact_provider_facts_name_only_their_declared_storage_slots() {
        let NativeStateFact::SyntheticSlots(map) =
            NativeStateFact::for_classfile_name("java/util/HashMap")
        else {
            panic!("HashMap has an exact native map provider");
        };
        assert!(map.contains(Syn::Map));
        assert!(!map.contains(Syn::Elems));

        let NativeStateFact::SyntheticSlots(entry) =
            NativeStateFact::for_classfile_name("java/util/Map$Entry")
        else {
            panic!("Map.Entry has an exact native entry provider");
        };
        assert!(entry.contains(Syn::Key));
        assert!(entry.contains(Syn::Val));
        assert!(!entry.contains(Syn::Map));

        let set = crate::fastutil::INT_SETS[0];
        let NativeStateFact::SyntheticSlots(sequence) = NativeStateFact::for_classfile_name(set)
        else {
            panic!("fastutil int sets have an exact native representation provider");
        };
        assert!(sequence.contains(Syn::Elems));
        assert!(!sequence.contains(Syn::Map));
    }

    #[test]
    fn missing_provider_contract_is_unknown_not_native_state_absence() {
        assert_eq!(
            NativeStateFact::for_classfile_name("net/minecraft/FooList"),
            NativeStateFact::Unknown,
        );
        assert_eq!(
            NativeStateFact::for_classfile_name("java/util/BitSet"),
            NativeStateFact::Unknown,
        );
    }

    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn exact_storage_providers_already_require_handle_representation() {
        let assert_handle_provider = |owner: &str| {
            assert!(
                NativeStateFact::for_classfile_name(owner).slots().is_some(),
                "missing direct storage fact for {owner}",
            );
            assert!(
                crate::opcodes::birth_native_backed_alloc(owner),
                "storage fact would change the current birth route for {owner}",
            );
        };

        for owner in crate::map::MAP_CLASSES
            .iter()
            .chain(crate::map::FASTUTIL_OBJMAPS)
            .chain(crate::map::FASTUTIL_OBJ2OBJ)
        {
            assert_handle_provider(owner);
        }
        for owner in crate::map::ENTRY_CLASSES {
            assert_handle_provider(owner);
        }
        for owner in crate::fastutil::INT_SETS {
            assert_handle_provider(owner);
        }
    }
}
