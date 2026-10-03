//! Class-file parser (JVMS chapter 4) for the core. Names are interned `Symbol`s; attribute bodies
//! the runtime reads lazily (annotations, signatures, inner classes, line numbers) stay as ranges
//! into the class bytes, which live as long as the class.

use crate::symbol::{modified_utf8_to_string, Symbol};

#[derive(Debug)]
pub struct ParseError(pub &'static str);

type Result<T> = std::result::Result<T, ParseError>;

/// One constant-pool slot. Indices are 1-based; the slot after a Long/Double is `Empty`.
#[derive(Clone, Copy, Debug)]
pub enum Cp {
    Empty,
    Utf8 { sym: Symbol, at: u32, len: u16 },
    Int(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(u16),
    String(u16),
    Field { class: u16, nat: u16 },
    Method { class: u16, nat: u16 },
    IMethod { class: u16, nat: u16 },
    NameAndType { name: u16, desc: u16 },
    MethodHandle { kind: u8, index: u16 },
    MethodType(u16),
    Dynamic { bsm: u16, nat: u16 },
    InvokeDynamic { bsm: u16, nat: u16 },
    Module(u16),
    Package(u16),
}

// Pools hold millions of entries on a large application.
const _: () = assert!(std::mem::size_of::<Cp>() == 16);

/// An attribute body the runtime may read later, as a range of the class bytes.
#[derive(Clone, Copy, Debug)]
pub struct Attr {
    pub name: Symbol,
    pub at: u32,
    pub len: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Handler {
    pub start: u16,
    pub end: u16,
    pub handler: u16,
    pub catch_type: u16,
}

#[derive(Debug)]
pub struct CodeInfo {
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: &'static [u8],
    pub handlers: Vec<Handler>,
    pub line_numbers: Option<Attr>,
}

#[derive(Debug)]
pub struct FieldInfo {
    pub access: u16,
    pub name: Symbol,
    pub desc: Symbol,
    pub constant_value: u16,
    pub attrs: Vec<Attr>,
}

#[derive(Debug)]
pub struct MethodInfo {
    pub access: u16,
    pub name: Symbol,
    pub desc: Symbol,
    pub code: Option<CodeInfo>,
    pub attrs: Vec<Attr>,
}

pub struct ClassFile {
    pub bytes: &'static [u8],
    pub major: u16,
    pub pool: Vec<Cp>,
    pub access: u16,
    pub name: Symbol,
    pub super_name: Option<Symbol>,
    pub interfaces: Vec<Symbol>,
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
    pub attrs: Vec<Attr>,
}

struct Reader {
    bytes: &'static [u8],
    pos: usize,
}

impl Reader {
    fn take(&mut self, n: usize) -> Result<&'static [u8]> {
        let s = self.bytes.get(self.pos..self.pos + n).ok_or(ParseError("truncated class file"))?;
        self.pos += n;
        Ok(s)
    }
    fn u1(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u2(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u4(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
}

/// Nanoseconds spent parsing class files (`RAVA_PROFILE`).
pub static PARSE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ClassFile {
    /// Parses `bytes`, which must outlive every class defined from them (they are leaked or
    /// memory-mapped by the caller).
    pub fn parse(bytes: &'static [u8]) -> Result<ClassFile> {
        let start = std::env::var_os("RAVA_PROFILE").is_some().then(std::time::Instant::now);
        let r = Self::parse_inner(bytes);
        if let Some(start) = start {
            PARSE_NS.fetch_add(start.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        r
    }

    fn parse_inner(bytes: &'static [u8]) -> Result<ClassFile> {
        let mut r = Reader { bytes, pos: 0 };
        if r.u4()? != 0xCAFE_BABE {
            return Err(ParseError("bad magic"));
        }
        let _minor = r.u2()?;
        let major = r.u2()?;
        let pool = parse_pool(&mut r)?;
        let mut cf = ClassFile {
            bytes,
            major,
            access: r.u2()?,
            name: Symbol::intern(""),
            super_name: None,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attrs: Vec::new(),
            pool,
        };
        cf.name = cf.class_name(r.u2()?).ok_or(ParseError("bad this_class"))?;
        let super_index = r.u2()?;
        cf.super_name = if super_index == 0 { None } else { Some(cf.class_name(super_index).ok_or(ParseError("bad super"))?) };
        for _ in 0..r.u2()? {
            let i = r.u2()?;
            cf.interfaces.push(cf.class_name(i).ok_or(ParseError("bad interface"))?);
        }
        for _ in 0..r.u2()? {
            let (access, name, desc) = (r.u2()?, cf.utf8(r.u2()?)?, cf.utf8(r.u2()?)?);
            let attrs = cf.parse_attrs(&mut r)?;
            let constant_value = attrs
                .iter()
                .find(|a| a.name == "ConstantValue")
                .map_or(0, |a| u16::from_be_bytes([bytes[a.at as usize], bytes[a.at as usize + 1]]));
            cf.fields.push(FieldInfo { access, name, desc, constant_value, attrs });
        }
        for _ in 0..r.u2()? {
            let (access, name, desc) = (r.u2()?, cf.utf8(r.u2()?)?, cf.utf8(r.u2()?)?);
            let mut attrs = cf.parse_attrs(&mut r)?;
            let code = match attrs.iter().position(|a| a.name == "Code") {
                Some(i) => Some(cf.parse_code(attrs.swap_remove(i))?),
                None => None,
            };
            cf.methods.push(MethodInfo { access, name, desc, code, attrs });
        }
        cf.attrs = cf.parse_attrs(&mut r)?;
        Ok(cf)
    }

    pub fn utf8(&self, index: u16) -> Result<Symbol> {
        match self.pool.get(index as usize) {
            Some(Cp::Utf8 { sym, .. }) => Ok(*sym),
            _ => Err(ParseError("expected Utf8 constant")),
        }
    }

    /// The raw modified-UTF-8 bytes of a Utf8 constant (for exact string literals).
    pub fn utf8_bytes(&self, index: u16) -> Option<&'static [u8]> {
        match self.pool.get(index as usize) {
            Some(Cp::Utf8 { at, len, .. }) => self.bytes.get(*at as usize..*at as usize + *len as usize),
            _ => None,
        }
    }

    pub fn class_name(&self, index: u16) -> Option<Symbol> {
        match self.pool.get(index as usize) {
            Some(Cp::Class(name)) => self.utf8(*name).ok(),
            _ => None,
        }
    }

    pub fn name_and_type(&self, index: u16) -> Option<(Symbol, Symbol)> {
        match self.pool.get(index as usize) {
            Some(Cp::NameAndType { name, desc }) => Some((self.utf8(*name).ok()?, self.utf8(*desc).ok()?)),
            _ => None,
        }
    }

    /// `(owner class, name, descriptor)` of a Field/Method/InterfaceMethod reference.
    pub fn member_ref(&self, index: u16) -> Option<(Symbol, Symbol, Symbol)> {
        let (class, nat) = match self.pool.get(index as usize)? {
            Cp::Field { class, nat } | Cp::Method { class, nat } | Cp::IMethod { class, nat } => (*class, *nat),
            _ => return None,
        };
        let (name, desc) = self.name_and_type(nat)?;
        Some((self.class_name(class)?, name, desc))
    }

    /// Entry `i` of the `BootstrapMethods` attribute: the method-handle constant and the
    /// static-argument constant indices.
    pub fn bootstrap_method(&self, i: u16) -> Option<(u16, Vec<u16>)> {
        let b = self.attr_bytes(self.attr("BootstrapMethods")?);
        let count = u16::from_be_bytes([b[0], b[1]]);
        let mut pos = 2;
        for n in 0..count {
            let handle = u16::from_be_bytes([b[pos], b[pos + 1]]);
            let argc = u16::from_be_bytes([b[pos + 2], b[pos + 3]]) as usize;
            if n == i {
                let args = (0..argc).map(|a| u16::from_be_bytes([b[pos + 4 + 2 * a], b[pos + 5 + 2 * a]])).collect();
                return Some((handle, args));
            }
            pos += 4 + 2 * argc;
        }
        None
    }

    pub fn attr(&self, name: &str) -> Option<Attr> {
        self.attrs.iter().copied().find(|a| a.name == name)
    }

    pub fn attr_bytes(&self, a: Attr) -> &'static [u8] {
        &self.bytes[a.at as usize..(a.at + a.len) as usize]
    }

    fn parse_attrs(&self, r: &mut Reader) -> Result<Vec<Attr>> {
        let n = r.u2()? as usize;
        let mut attrs = Vec::with_capacity(n);
        for _ in 0..n {
            let name = self.utf8(r.u2()?)?;
            let len = r.u4()?;
            let at = r.pos as u32;
            r.take(len as usize)?;
            attrs.push(Attr { name, at, len });
        }
        Ok(attrs)
    }

    fn parse_code(&self, a: Attr) -> Result<CodeInfo> {
        let mut r = Reader { bytes: self.bytes, pos: a.at as usize };
        let max_stack = r.u2()?;
        let max_locals = r.u2()?;
        let len = r.u4()? as usize;
        let code = r.take(len)?;
        let mut handlers = Vec::new();
        for _ in 0..r.u2()? {
            handlers.push(Handler { start: r.u2()?, end: r.u2()?, handler: r.u2()?, catch_type: r.u2()? });
        }
        let attrs = self.parse_attrs(&mut r)?;
        let line_numbers = attrs.into_iter().find(|a| a.name == "LineNumberTable");
        Ok(CodeInfo { max_stack, max_locals, code, handlers, line_numbers })
    }
}

/// Type-descriptor constant indices of the annotations in a `Runtime*Annotations` body.
pub fn annotation_types(body: &[u8]) -> Vec<u16> {
    fn u2(b: &[u8], p: usize) -> u16 {
        u16::from_be_bytes([b[p], b[p + 1]])
    }
    fn skip_annotation(b: &[u8], mut p: usize, out: Option<&mut Vec<u16>>) -> usize {
        if let Some(out) = out {
            out.push(u2(b, p));
        }
        let pairs = u2(b, p + 2);
        p += 4;
        for _ in 0..pairs {
            p = skip_value(b, p + 2);
        }
        p
    }
    fn skip_value(b: &[u8], p: usize) -> usize {
        match b[p] {
            b'e' => p + 5,
            b'@' => skip_annotation(b, p + 1, None),
            b'[' => (0..u2(b, p + 1)).fold(p + 3, |q, _| skip_value(b, q)),
            _ => p + 3,
        }
    }
    let mut out = Vec::new();
    if body.len() < 2 {
        return out;
    }
    let mut p = 2;
    for _ in 0..u2(body, 0) {
        p = skip_annotation(body, p, Some(&mut out));
    }
    out
}

/// The constant-valued elements of a `RuntimeVisibleAnnotations` body's annotations: (type
/// descriptor index, element name index, value tag, constant index) for each.
pub fn annotation_constants(body: &[u8]) -> Vec<(u16, u16, u8, u16)> {
    fn u2(b: &[u8], p: usize) -> u16 {
        u16::from_be_bytes([b[p], b[p + 1]])
    }
    fn skip_value(b: &[u8], p: usize) -> usize {
        match b[p] {
            b'e' => p + 5,
            b'@' => (0..u2(b, p + 3)).fold(p + 5, |q, _| skip_value(b, q + 2)),
            b'[' => (0..u2(b, p + 1)).fold(p + 3, |q, _| skip_value(b, q)),
            _ => p + 3,
        }
    }
    let mut out = Vec::new();
    if body.len() < 2 {
        return out;
    }
    let mut p = 2;
    for _ in 0..u2(body, 0) {
        let ty = u2(body, p);
        let pairs = u2(body, p + 2);
        p += 4;
        for _ in 0..pairs {
            let (name, tag) = (u2(body, p), body[p + 2]);
            if !matches!(tag, b'e' | b'@' | b'[' | b'c') {
                out.push((ty, name, tag, u2(body, p + 3)));
            }
            p = skip_value(body, p + 2);
        }
    }
    out
}

fn parse_pool(r: &mut Reader) -> Result<Vec<Cp>> {
    let count = r.u2()? as usize;
    let mut pool = Vec::with_capacity(count);
    pool.push(Cp::Empty);
    while pool.len() < count {
        let tag = r.u1()?;
        let entry = match tag {
            1 => {
                let len = r.u2()?;
                let at = r.pos as u32;
                let raw = r.take(len as usize)?;
                let sym = if raw.iter().all(|&b| b != 0 && b < 0x80) {
                    // SAFETY: non-NUL ASCII is valid UTF-8 and identical in modified UTF-8.
                    Symbol::intern(unsafe { std::str::from_utf8_unchecked(raw) })
                } else {
                    Symbol::intern(&modified_utf8_to_string(raw))
                };
                Cp::Utf8 { sym, at, len }
            }
            3 => Cp::Int(r.u4()? as i32),
            4 => Cp::Float(f32::from_bits(r.u4()?)),
            5 => Cp::Long(((r.u4()? as u64) << 32 | r.u4()? as u64) as i64),
            6 => Cp::Double(f64::from_bits((r.u4()? as u64) << 32 | r.u4()? as u64)),
            7 => Cp::Class(r.u2()?),
            8 => Cp::String(r.u2()?),
            9 => Cp::Field { class: r.u2()?, nat: r.u2()? },
            10 => Cp::Method { class: r.u2()?, nat: r.u2()? },
            11 => Cp::IMethod { class: r.u2()?, nat: r.u2()? },
            12 => Cp::NameAndType { name: r.u2()?, desc: r.u2()? },
            15 => Cp::MethodHandle { kind: r.u1()?, index: r.u2()? },
            16 => Cp::MethodType(r.u2()?),
            17 => Cp::Dynamic { bsm: r.u2()?, nat: r.u2()? },
            18 => Cp::InvokeDynamic { bsm: r.u2()?, nat: r.u2()? },
            19 => Cp::Module(r.u2()?),
            20 => Cp::Package(r.u2()?),
            _ => return Err(ParseError("unknown constant tag")),
        };
        pool.push(entry);
        if matches!(tag, 5 | 6) {
            pool.push(Cp::Empty);
        }
    }
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jimage::JImage;

    #[test]
    fn parses_hashmap_from_java_base() {
        let Some(home) = crate::java_home() else { return };
        let img: &'static JImage = Box::leak(Box::new(JImage::open_java_home(home).unwrap()));
        let cf = ClassFile::parse(img.class_bytes("java/util/HashMap").unwrap()).unwrap();
        assert_eq!(cf.name, "java/util/HashMap");
        assert_eq!(cf.super_name.unwrap(), "java/util/AbstractMap");
        let get = cf.methods.iter().find(|m| m.name == "get" && m.desc == "(Ljava/lang/Object;)Ljava/lang/Object;").unwrap();
        assert!(get.code.as_ref().unwrap().code.len() > 5);
        let serial = cf.fields.iter().find(|f| f.name == "serialVersionUID").unwrap();
        assert!(matches!(cf.pool[serial.constant_value as usize], Cp::Long(362498820763181265)));
    }
}
