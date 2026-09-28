//! A single boundary for reading and writing reference-array representations.
//!
//! Legacy arrays are heap-table handles.  Arena reference arrays are inline-cell objects whose
//! class metadata is the representation proof.  Keeping that distinction here prevents callers
//! from interpreting an arena pointer as an array-table index while leaving Java type checks to
//! the caller that has the corresponding bytecode/class context.

use crate::arena::ArenaPtr;
use crate::heap::Heap;
use crate::value::Value;
use std::sync::Arc;

/// A component descriptor borrowed from the legacy heap interner or shared from arena CID data.
pub(crate) enum ArrayComponent<'a> {
    Borrowed(&'a str),
    Shared(Arc<str>),
}

impl AsRef<str> for ArrayComponent<'_> {
    fn as_ref(&self) -> &str {
        match self {
            Self::Borrowed(descriptor) => descriptor,
            Self::Shared(descriptor) => descriptor,
        }
    }
}

/// The two representations accepted by the reference-array boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayRef {
    /// The legacy index into `Heap::arrays`.
    Handle(u32),
    /// An arena object whose class metadata proves it is a reference array.
    Arena(ArenaPtr),
}

impl ArrayRef {
    /// Classify a runtime value without using names or guessing from the pointer shape.
    ///
    /// `Value::Array` remains the legacy representation, including for callers that will do
    /// their own component/type check.  An arena object is admitted only by the exact CID-backed
    /// reference-array predicate.
    #[inline]
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Array(handle) => Some(Self::Handle(*handle)),
            Value::Obj(pointer) if crate::nextgen::is_arena_ref_array(*pointer) => {
                Some(Self::Arena(*pointer))
            }
            _ => None,
        }
    }

    /// Return the fixed array length, or `None` for a dead/malformed legacy handle.
    #[inline]
    pub(crate) fn len(self, heap: &Heap) -> Option<usize> {
        match self {
            Self::Handle(handle) => heap.array_len(handle),
            // The Arena variant is minted only by `from_value`, which already validated the
            // address and exact descriptor-bearing CID. Avoid repeating that metadata lookup on
            // every element operation.
            Self::Arena(pointer) => Some(crate::nextgen::arena_nfields(pointer) as usize),
        }
    }

    /// Read one element as the raw runtime `Value` used by the interpreter.
    #[inline]
    pub(crate) fn get(self, heap: &Heap, index: usize) -> Option<Value> {
        match self {
            Self::Handle(handle) => heap.array_get(handle, index),
            Self::Arena(pointer) => self.get_arena(pointer, index),
        }
    }

    /// Store one element after an explicit bounds check.
    ///
    /// The caller owns Java assignability/type checking.  This method only performs the
    /// representation-specific cell encoding and the fixed-length bounds check.
    #[inline]
    pub(crate) fn store(self, heap: &mut Heap, index: usize, value: Value) -> bool {
        self.store_encoded(heap, index, &value)
    }

    /// Store one element using the representation's existing encoding contract.
    #[inline]
    pub(crate) fn store_encoded(self, heap: &mut Heap, index: usize, value: &Value) -> bool {
        match self {
            Self::Handle(handle) => {
                if heap.array_len(handle).is_none_or(|length| index >= length) {
                    return false;
                }
                heap.array_set(handle, index, value.clone());
                true
            }
            Self::Arena(pointer) => self.store_arena(pointer, index, value),
        }
    }

    /// Return the exact component descriptor retained by this array representation.
    ///
    /// The legacy table owns an interned descriptor in `Heap`; the arena class registry owns an
    /// immutable descriptor by CID.  Both are returned as an `Arc<str>` so the facade does not
    /// expose either backing store's lifetime or lock.
    #[inline]
    pub(crate) fn component_descriptor<'a>(self, heap: &'a Heap) -> Option<ArrayComponent<'a>> {
        match self {
            Self::Handle(handle) => heap.array_element(handle).map(ArrayComponent::Shared),
            Self::Arena(pointer) if crate::nextgen::is_arena_ref_array(pointer) => {
                crate::nextgen::arena_ref_array_component(pointer).map(ArrayComponent::Shared)
            }
            Self::Arena(_) => None,
        }
    }

    /// Materialize an arena reference array into the legacy handle table.
    ///
    /// The descriptor is retained by the arena CID, so read it once and intern that exact
    /// component before allocating the destination.  The copy has no safepoint-capable operation:
    /// arena decoding is a direct cell read and legacy stores only encode into the already-created
    /// array (a boxed value may extend `array_box`, but does not collect).  A direct self-reference
    /// must be retargeted to the new handle rather than leaving a pointer into the source object.
    pub(crate) fn materialize(self, heap: &mut Heap) -> Option<u32> {
        let Self::Arena(source) = self else {
            return match self {
                Self::Handle(handle) if heap.array_len(handle).is_some() => Some(handle),
                _ => None,
            };
        };

        let component = crate::nextgen::arena_ref_array_component(source)?;
        let len = crate::nextgen::arena_nfields(source) as usize;
        let element_id = heap.intern_array_element(component.as_ref());
        let handle = heap.alloc_ref_array_by_id(element_id, len);

        for index in 0..len {
            let value = self.get(heap, index)?;
            let value = match value {
                Value::Obj(pointer) if pointer == source => Value::Array(handle),
                value => value,
            };
            heap.array_set(handle, index, value);
        }
        Some(handle)
    }

    #[inline]
    fn get_arena(self, pointer: ArenaPtr, index: usize) -> Option<Value> {
        if !matches!(self, Self::Arena(_))
            || index >= crate::nextgen::arena_nfields(pointer) as usize
        {
            return None;
        }
        #[cfg(feature = "next-gen-gc")]
        {
            let cell = crate::nextgen::arena_cell(pointer, index);
            Some(crate::opcodes::decode_arena_cell(
                cell,
                crate::heap::fieldkinds::FK_REF,
            ))
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            let _ = pointer;
            None
        }
    }

    #[inline]
    fn store_arena(self, pointer: ArenaPtr, index: usize, value: &Value) -> bool {
        if !matches!(self, Self::Arena(_))
            || index >= crate::nextgen::arena_nfields(pointer) as usize
        {
            return false;
        }
        #[cfg(feature = "next-gen-gc")]
        {
            // Keep encoding and the remembered-set barrier in one shared path.  Encoding may
            // materialize a raw arena pointer from Value::Str or a pinned Value::Ref, so checking
            // only the pre-encoded Value::Obj form misses old→young stores.
            crate::opcodes::store_arena_ref_cell(pointer, index, value);
            true
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            let _ = (pointer, index, value);
            false
        }
    }
}
