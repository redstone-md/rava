//! JVM basic types and method-descriptor shapes.

use crate::symbol::Symbol;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum BasicType {
    Boolean,
    Char,
    Float,
    Double,
    Byte,
    Short,
    Int,
    Long,
    Reference,
    Void,
}

impl BasicType {
    pub fn from_desc(c: u8) -> BasicType {
        match c {
            b'Z' => BasicType::Boolean,
            b'C' => BasicType::Char,
            b'F' => BasicType::Float,
            b'D' => BasicType::Double,
            b'B' => BasicType::Byte,
            b'S' => BasicType::Short,
            b'I' => BasicType::Int,
            b'J' => BasicType::Long,
            b'V' => BasicType::Void,
            _ => BasicType::Reference,
        }
    }

    /// `T_*` codes used by `newarray` (JVMS 6.5).
    pub fn from_newarray_code(code: u8) -> Option<BasicType> {
        Some(match code {
            4 => BasicType::Boolean,
            5 => BasicType::Char,
            6 => BasicType::Float,
            7 => BasicType::Double,
            8 => BasicType::Byte,
            9 => BasicType::Short,
            10 => BasicType::Int,
            11 => BasicType::Long,
            _ => return None,
        })
    }

    /// Bytes an instance field or array element of this type occupies.
    pub fn size(self) -> u32 {
        match self {
            BasicType::Boolean | BasicType::Byte => 1,
            BasicType::Char | BasicType::Short => 2,
            BasicType::Int | BasicType::Float => 4,
            BasicType::Long | BasicType::Double => 8,
            BasicType::Reference => crate::heap::REF_SIZE,
            BasicType::Void => 0,
        }
    }

    /// Local-variable / operand-stack slots (JVMS computational category).
    pub fn slots(self) -> u16 {
        match self {
            BasicType::Long | BasicType::Double => 2,
            BasicType::Void => 0,
            _ => 1,
        }
    }

    pub fn is_ref(self) -> bool {
        self == BasicType::Reference
    }

    pub fn desc_char(self) -> char {
        b"ZCFDBSIJLV"[self as usize] as char
    }
}

/// A parsed method descriptor.
#[derive(Clone, Debug)]
pub struct MethodSig {
    pub params: Box<[BasicType]>,
    pub ret: BasicType,
    /// Argument slots, excluding the receiver.
    pub arg_slots: u16,
}

impl MethodSig {
    pub fn parse(desc: Symbol) -> MethodSig {
        let b = desc.as_str().as_bytes();
        let mut params = Vec::new();
        let mut i = 1;
        while i < b.len() && b[i] != b')' {
            params.push(BasicType::from_desc(b[i]));
            i = skip_field_desc(b, i);
        }
        let ret = BasicType::from_desc(*b.get(i + 1).unwrap_or(&b'V'));
        let arg_slots = params.iter().map(|t| t.slots()).sum();
        MethodSig { params: params.into_boxed_slice(), ret, arg_slots }
    }
}

/// Index just past the field descriptor that starts at `i`.
pub fn skip_field_desc(b: &[u8], mut i: usize) -> usize {
    while b[i] == b'[' {
        i += 1;
    }
    if b[i] == b'L' {
        while b[i] != b';' {
            i += 1;
        }
    }
    i + 1
}

/// The class name a field descriptor refers to: `Ljava/lang/String;` → `java/lang/String`,
/// arrays keep their descriptor form (`[I`). `None` for primitives.
pub fn desc_to_class_name(desc: &str) -> Option<&str> {
    match desc.as_bytes().first()? {
        b'L' => Some(&desc[1..desc.len() - 1]),
        b'[' => Some(desc),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_descriptors() {
        let s = MethodSig::parse(Symbol::intern("(IJ[Ljava/lang/String;DLjava/lang/Object;)Z"));
        use BasicType::*;
        assert_eq!(&*s.params, &[Int, Long, Reference, Double, Reference]);
        assert_eq!(s.ret, Boolean);
        assert_eq!(s.arg_slots, 7);
        assert_eq!(desc_to_class_name("Ljava/lang/String;"), Some("java/lang/String"));
    }
}
