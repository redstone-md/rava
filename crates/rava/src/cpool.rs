//! A parsed constant pool, one word per slot (pools hold millions of entries on a large
//! application): a tag in the top byte, the operands below it, a `Utf8` constant's symbol as
//! a pointer, and a `Long`/`Double` split over its two slots as the class file does. Entries
//! read back as `Cp` values.

use crate::classfile::Cp;
use crate::symbol::Symbol;

/// `Cp::Utf8::at` of a constant whose class-file bytes are its symbol's text (no NUL,
/// surrogate or supplementary character, where modified UTF-8 differs from UTF-8).
pub const TEXT: u32 = u32::MAX;

const SHIFT: u32 = 56;
const PAYLOAD: u64 = (1 << SHIFT) - 1;
/// The second slot of a `Long`/`Double`: the low 32 bits (reads as `Empty`).
const LOW: u64 = 0xff;

pub struct Pool {
    words: Box<[u64]>,
    /// `Utf8` constants whose bytes differ from their symbol's text, by index: `(index, at,
    /// len)` of the bytes in the class file's buffer.
    raw: Box<[(u16, u32, u16)]>,
}

impl Pool {
    /// The pool of `entries` (as parsed: a `Utf8` constant's `at` locates its bytes; `same`
    /// tells whether they are its symbol's text).
    pub fn new(entries: &[(Cp, bool)]) -> Pool {
        let mut raw = Vec::new();
        let mut words = Vec::with_capacity(entries.len());
        for (i, &(cp, same)) in entries.iter().enumerate() {
            let tagged = |tag: u64, payload: u64| tag << SHIFT | payload & PAYLOAD;
            let pair = |a: u16, b: u16| (a as u64) << 16 | b as u64;
            let w = match cp {
                Cp::Empty => {
                    // The slot after a Long/Double keeps its low half.
                    if let Some(&(Cp::Long(_) | Cp::Double(_), _)) = i.checked_sub(1).and_then(|p| entries.get(p)) {
                        let bits = match entries[i - 1].0 {
                            Cp::Long(v) => v as u64,
                            Cp::Double(v) => v.to_bits(),
                            _ => unreachable!(),
                        };
                        tagged(LOW, bits & 0xffff_ffff)
                    } else {
                        0
                    }
                }
                Cp::Utf8 { sym, at, len } => {
                    if !same {
                        raw.push((i as u16, at, len));
                    }
                    tagged(1, sym.to_raw() as u64)
                }
                Cp::Int(v) => tagged(3, v as u32 as u64),
                Cp::Float(v) => tagged(4, v.to_bits() as u64),
                Cp::Long(v) => tagged(5, v as u64 >> 32),
                Cp::Double(v) => tagged(6, v.to_bits() >> 32),
                Cp::Class(x) => tagged(7, x as u64),
                Cp::String(x) => tagged(8, x as u64),
                Cp::Field { class, nat } => tagged(9, pair(class, nat)),
                Cp::Method { class, nat } => tagged(10, pair(class, nat)),
                Cp::IMethod { class, nat } => tagged(11, pair(class, nat)),
                Cp::NameAndType { name, desc } => tagged(12, pair(name, desc)),
                Cp::MethodHandle { kind, index } => tagged(15, pair(kind as u16, index)),
                Cp::MethodType(x) => tagged(16, x as u64),
                Cp::Dynamic { bsm, nat } => tagged(17, pair(bsm, nat)),
                Cp::InvokeDynamic { bsm, nat } => tagged(18, pair(bsm, nat)),
                Cp::Module(x) => tagged(19, x as u64),
                Cp::Package(x) => tagged(20, x as u64),
            };
            words.push(w);
        }
        Pool { words: words.into_boxed_slice(), raw: raw.into_boxed_slice() }
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Entry `i`.
    pub fn at(&self, i: usize) -> Cp {
        self.get(i).expect("a constant-pool index in range")
    }

    pub fn get(&self, i: usize) -> Option<Cp> {
        let w = *self.words.get(i)?;
        let p = w & PAYLOAD;
        let (hi, lo) = ((p >> 16) as u16, p as u16);
        let joined = || (p << 32) | (self.words[i + 1] & 0xffff_ffff);
        Some(match w >> SHIFT {
            0 | LOW => Cp::Empty,
            1 => {
                // SAFETY: written from a symbol by `new`.
                let sym = unsafe { Symbol::from_raw(p as usize) };
                match self.raw.binary_search_by_key(&(i as u16), |r| r.0) {
                    Ok(r) => Cp::Utf8 { sym, at: self.raw[r].1, len: self.raw[r].2 },
                    Err(_) => Cp::Utf8 { sym, at: TEXT, len: sym.as_str().len() as u16 },
                }
            }
            3 => Cp::Int(p as u32 as i32),
            4 => Cp::Float(f32::from_bits(p as u32)),
            5 => Cp::Long(joined() as i64),
            6 => Cp::Double(f64::from_bits(joined())),
            7 => Cp::Class(lo),
            8 => Cp::String(lo),
            9 => Cp::Field { class: hi, nat: lo },
            10 => Cp::Method { class: hi, nat: lo },
            11 => Cp::IMethod { class: hi, nat: lo },
            12 => Cp::NameAndType { name: hi, desc: lo },
            15 => Cp::MethodHandle { kind: hi as u8, index: lo },
            16 => Cp::MethodType(lo),
            17 => Cp::Dynamic { bsm: hi, nat: lo },
            18 => Cp::InvokeDynamic { bsm: hi, nat: lo },
            19 => Cp::Module(lo),
            _ => Cp::Package(lo),
        })
    }

    /// Where the bytes of the `Utf8` constants that are not their symbol's text are, for
    /// moving them (`ClassFile::compact`): `(at, len)` of each.
    pub fn raw_utf8_mut(&mut self) -> impl Iterator<Item = (&mut u32, u16)> {
        self.raw.iter_mut().map(|r| (&mut r.1, r.2))
    }
}
