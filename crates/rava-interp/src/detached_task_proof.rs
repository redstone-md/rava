use crate::jit::{self, JT};
use crate::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PrimitiveKind {
    Int,
    Long,
    Float,
    Double,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LambdaArgument {
    Primitive(PrimitiveKind),
    Reference,
}

impl LambdaArgument {
    pub(super) fn slots(self) -> usize {
        match self {
            Self::Primitive(kind) => kind.slots(),
            Self::Reference => 1,
        }
    }
}

impl PrimitiveKind {
    pub(super) fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'Z' | b'B' | b'C' | b'S' | b'I' => Some(Self::Int),
            b'J' => Some(Self::Long),
            b'F' => Some(Self::Float),
            b'D' => Some(Self::Double),
            _ => None,
        }
    }

    pub(super) fn slots(self) -> usize {
        usize::from(matches!(self, Self::Long | Self::Double)) + 1
    }

    pub(super) fn jit_type(self) -> JT {
        match self {
            Self::Int => JT::Int,
            Self::Long => JT::Long,
            Self::Float => JT::Float,
            Self::Double => JT::Double,
        }
    }

    fn return_opcode(self) -> u8 {
        match self {
            Self::Int => 0xac,
            Self::Long => 0xad,
            Self::Float => 0xae,
            Self::Double => 0xaf,
        }
    }

    pub(super) fn encode(self, value: Value) -> Option<i64> {
        match (self, value) {
            (Self::Int, Value::Int(value)) => Some(value as i64),
            (Self::Long, Value::Long(value)) => Some(value),
            (Self::Float, Value::Float(value)) => Some(value.to_bits() as i64),
            (Self::Double, Value::Double(value)) => Some(value.to_bits() as i64),
            _ => None,
        }
    }

    pub(super) fn encode_field(self, value: Value) -> Option<i64> {
        matches!(value, Value::Null)
            .then_some(0)
            .or_else(|| self.encode(value))
    }

    pub(super) fn decode(self, bits: i64) -> Value {
        match self {
            Self::Int => Value::Int(bits as i32),
            Self::Long => Value::Long(bits),
            Self::Float => Value::Float(f32::from_bits(bits as u32)),
            Self::Double => Value::Double(f64::from_bits(bits as u64)),
        }
    }
}

pub(super) fn primitive_field_kind(descriptor: &str) -> Option<PrimitiveKind> {
    let bytes = descriptor.as_bytes();
    (bytes.len() == 1)
        .then(|| PrimitiveKind::from_byte(bytes[0]))
        .flatten()
}

pub(super) fn primitive_signature(descriptor: &str) -> Option<(Vec<PrimitiveKind>, PrimitiveKind)> {
    let (arguments, result) = split_signature(descriptor)?;
    Some((
        arguments,
        PrimitiveKind::from_byte(*result.as_bytes().first()?)?,
    ))
    .filter(|_| result.len() == 1)
}

pub(super) fn lambda_wrapper_signature(descriptor: &str) -> Option<(Vec<LambdaArgument>, &str)> {
    let bytes = descriptor.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut arguments = Vec::new();
    let mut index = 1usize;
    while bytes.get(index) != Some(&b')') {
        if let Some(kind) = PrimitiveKind::from_byte(*bytes.get(index)?) {
            arguments.push(LambdaArgument::Primitive(kind));
            index += 1;
            continue;
        }
        if bytes.get(index) != Some(&b'L') {
            return None;
        }
        let start = index;
        while bytes.get(index) != Some(&b';') {
            index += 1;
        }
        index += 1;
        reference_descriptor(descriptor.get(start..index)?)?;
        arguments.push(LambdaArgument::Reference);
    }
    let result = descriptor.get(index + 1..)?;
    reference_descriptor(result).map(|result| (arguments, result))
}

fn split_signature(descriptor: &str) -> Option<(Vec<PrimitiveKind>, &str)> {
    let bytes = descriptor.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut arguments = Vec::new();
    let mut index = 1usize;
    while bytes.get(index) != Some(&b')') {
        arguments.push(PrimitiveKind::from_byte(*bytes.get(index)?)?);
        index += 1;
    }
    Some((arguments, descriptor.get(index + 1..)?))
}

fn reference_descriptor(descriptor: &str) -> Option<&str> {
    let bytes = descriptor.as_bytes();
    (bytes.len() > 2
        && bytes.first() == Some(&b'L')
        && bytes.last() == Some(&b';')
        && !bytes[1..bytes.len() - 1]
            .iter()
            .any(|byte| matches!(byte, b'(' | b')' | b'[' | b';')))
    .then_some(descriptor)
}

pub(super) fn reference_field_owner(descriptor: &str) -> Option<&str> {
    reference_descriptor(descriptor)?;
    descriptor.get(1..descriptor.len() - 1)
}

pub(super) fn lambda_reference_argument_owner(
    descriptor: &str,
    target_capture: usize,
) -> Option<&str> {
    let bytes = descriptor.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut index = 1usize;
    let mut capture = 0usize;
    while bytes.get(index) != Some(&b')') {
        if PrimitiveKind::from_byte(*bytes.get(index)?).is_some() {
            index += 1;
        } else {
            let start = index;
            while bytes.get(index) != Some(&b';') {
                index += 1;
            }
            index += 1;
            let owner = reference_field_owner(descriptor.get(start..index)?)?;
            if capture == target_capture {
                return Some(owner);
            }
        }
        capture += 1;
    }
    None
}

pub(super) fn post_result(descriptor: &str) -> Option<&str> {
    let bytes = descriptor.as_bytes();
    if bytes.first() != Some(&b'(')
        || PrimitiveKind::from_byte(*bytes.get(1)?).is_none()
        || bytes.get(2) != Some(&b')')
    {
        return None;
    }
    reference_descriptor(descriptor.get(3..)?)
}

pub(super) fn post_signature(descriptor: &str, input: PrimitiveKind) -> bool {
    PrimitiveKind::from_byte(*descriptor.as_bytes().get(1).unwrap_or(&0)) == Some(input)
        && post_result(descriptor).is_some()
}

pub(super) fn primitive_load_len(
    bytecode: &[u8],
    pc: usize,
    kind: PrimitiveKind,
    local: usize,
) -> Option<usize> {
    let generic = match kind {
        PrimitiveKind::Int => 0x15,
        PrimitiveKind::Long => 0x16,
        PrimitiveKind::Float => 0x17,
        PrimitiveKind::Double => 0x18,
    };
    let compact = match kind {
        PrimitiveKind::Int => 0x1a,
        PrimitiveKind::Long => 0x1e,
        PrimitiveKind::Float => 0x22,
        PrimitiveKind::Double => 0x26,
    };
    match *bytecode.get(pc)? {
        opcode if opcode == generic && usize::from(*bytecode.get(pc + 1)?) == local => Some(2),
        opcode if local < 4 && opcode == compact + local as u8 => Some(1),
        _ => None,
    }
}

pub(super) fn reference_load_len(bytecode: &[u8], pc: usize, local: usize) -> Option<usize> {
    match *bytecode.get(pc)? {
        0x19 if usize::from(*bytecode.get(pc + 1)?) == local => Some(2),
        opcode if local < 4 && opcode == 0x2a + local as u8 => Some(1),
        _ => None,
    }
}

pub(super) fn lambda_input_load(
    bytecode: &[u8],
    pc: usize,
    arguments: &[LambdaArgument],
) -> Option<(usize, LambdaArgument, usize)> {
    let mut local = 0usize;
    for (capture, argument) in arguments.iter().copied().enumerate() {
        let length = match argument {
            LambdaArgument::Primitive(kind) => primitive_load_len(bytecode, pc, kind, local),
            LambdaArgument::Reference => reference_load_len(bytecode, pc, local),
        };
        if let Some(length) = length {
            return Some((capture, argument, length));
        }
        local += argument.slots();
    }
    None
}

pub(super) fn detachable_body(code: &rava_classfile::Code, result: PrimitiveKind) -> bool {
    if !code.exceptions.is_empty() || code.bytecode.is_empty() {
        return false;
    }
    let mut pc = 0usize;
    let mut saw_return = false;
    while pc < code.bytecode.len() {
        let opcode = code.bytecode[pc];
        let allowed = matches!(
            opcode,
            0x00 | 0x02..=0x11 | 0x15..=0x18 | 0x1a..=0x29 | 0x36..=0x39
                | 0x3b..=0x4a | 0x57..=0x6b | 0x74..=0x98 | 0x99..=0xa4
                | 0xa7 | 0xac..=0xaf
        );
        if !allowed || matches!(opcode, 0x6c..=0x73) {
            return false;
        }
        if matches!(opcode, 0xac..=0xaf) {
            if opcode != result.return_opcode() {
                return false;
            }
            saw_return = true;
        }
        pc += match jit::insn_len_at(&code.bytecode, pc) {
            Some(length) => length,
            None => return false,
        };
    }
    saw_return && jit::find_leaders(&code.bytecode).is_some()
}
