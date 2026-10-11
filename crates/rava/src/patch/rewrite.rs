//! In-place class file edits for patches. No edit changes code, so branch offsets, exception
//! tables and stack maps stay valid: new constants are appended to the pool, new fields to
//! the field table, access flags are rewritten, and a call instruction becomes `invokestatic`
//! of the same length (an `invokeinterface` is padded with `nop`s).

use std::collections::HashMap;

const UTF8: u8 = 1;
const CLASS: u8 = 7;
const METHODREF: u8 = 10;
const NAME_AND_TYPE: u8 = 12;

const ACC_PUBLIC: u16 = 0x0001;
const ACC_PRIVATE: u16 = 0x0002;
const ACC_PROTECTED: u16 = 0x0004;
const ACC_STATIC: u16 = 0x0008;
const ACC_FINAL: u16 = 0x0010;

const INVOKEVIRTUAL: u8 = 0xb6;
const INVOKESPECIAL: u8 = 0xb7;
const INVOKESTATIC: u8 = 0xb8;
const INVOKEINTERFACE: u8 = 0xb9;
const NOP: u8 = 0x00;

/// A member reference as text: `owner`, `name`, `descriptor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemberRef {
    pub owner: String,
    pub name: String,
    pub desc: String,
}

/// One field or method of the class: where its access flags are, its name and descriptor,
/// and (for a method with code) where its bytecode is.
struct Member {
    flags_at: usize,
    name: String,
    desc: String,
    code: Option<(usize, usize)>,
}

/// A parsed view of a class file's bytes, with the edits made so far.
pub(crate) struct ClassBytes {
    bytes: Vec<u8>,
    /// Offset of each constant pool entry (index 0 and the second slot of a long are 0).
    cp: Vec<usize>,
    cp_end: usize,
    /// Constants appended by edits, serialized, and the pool size with them.
    extra: Vec<u8>,
    count: usize,
    utf8_index: HashMap<String, u16>,
    class_flags_at: usize,
    pub name: String,
    fields: Vec<Member>,
    methods: Vec<Member>,
    /// Where the fields count is, and where the fields end.
    fields_at: usize,
    fields_end: usize,
    /// Serialized `field_info`s of added fields.
    added: Vec<u8>,
    added_count: u16,
}

#[derive(Debug)]
pub(crate) struct Malformed;

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u1(&mut self) -> Result<u8, Malformed> {
        let v = *self.b.get(self.at).ok_or(Malformed)?;
        self.at += 1;
        Ok(v)
    }
    fn u2(&mut self) -> Result<u16, Malformed> {
        Ok((self.u1()? as u16) << 8 | self.u1()? as u16)
    }
    fn u4(&mut self) -> Result<u32, Malformed> {
        Ok((self.u2()? as u32) << 16 | self.u2()? as u32)
    }
    fn skip(&mut self, n: usize) -> Result<(), Malformed> {
        self.at = self.at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or(Malformed)?;
        Ok(())
    }
}

/// The internal name of the class in `bytes`, without parsing the rest.
pub(crate) fn class_name(bytes: &[u8]) -> Option<String> {
    ClassBytes::parse(bytes.to_vec()).ok().map(|c| c.name)
}

impl ClassBytes {
    pub(crate) fn parse(bytes: Vec<u8>) -> Result<Self, Malformed> {
        let mut r = Reader { b: &bytes, at: 8 };
        let count = r.u2()? as usize;
        let mut cp = vec![0; count];
        let mut i = 1;
        while i < count {
            cp[i] = r.at;
            let width = match r.u1()? {
                UTF8 => r.u2()? as usize,
                3 | 4 | 9 | 10 | 11 | NAME_AND_TYPE | 17 | 18 => 4,
                5 | 6 => {
                    i += 1;
                    8
                }
                CLASS | 8 | 16 | 19 | 20 => 2,
                15 => 3,
                _ => return Err(Malformed),
            };
            r.skip(width)?;
            i += 1;
        }
        let cp_end = r.at;
        let mut c = ClassBytes {
            bytes: Vec::new(),
            cp,
            cp_end,
            extra: Vec::new(),
            count,
            utf8_index: HashMap::new(),
            class_flags_at: cp_end,
            name: String::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            fields_at: 0,
            fields_end: 0,
            added: Vec::new(),
            added_count: 0,
        };
        c.bytes = bytes;
        for i in 1..count {
            if c.cp[i] != 0 && c.bytes[c.cp[i]] == UTF8 {
                let s = c.utf8(i as u16)?;
                c.utf8_index.entry(s).or_insert(i as u16);
            }
        }
        let mut r = Reader { b: &c.bytes, at: cp_end + 2 };
        let this = r.u2()?;
        c.name = c.class_ref(this)?;
        r.skip(2)?;
        let interfaces = r.u2()? as usize;
        r.skip(2 * interfaces)?;
        let mut at = r.at;
        c.fields_at = at;
        c.fields = c.members(&mut at)?;
        c.fields_end = at;
        c.methods = c.members(&mut at)?;
        Ok(c)
    }

    fn members(&self, at: &mut usize) -> Result<Vec<Member>, Malformed> {
        let mut r = Reader { b: &self.bytes, at: *at };
        let n = r.u2()?;
        let mut out = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let flags_at = r.at;
            r.skip(2)?;
            let (name, desc) = (self.utf8(r.u2()?)?, self.utf8(r.u2()?)?);
            let mut code = None;
            for _ in 0..r.u2()? {
                let attr = self.utf8(r.u2()?)?;
                let len = r.u4()? as usize;
                if attr == "Code" {
                    // max_stack, max_locals, code_length, then the code.
                    let mut c = Reader { b: &self.bytes, at: r.at + 4 };
                    let code_len = c.u4()? as usize;
                    c.skip(code_len)?;
                    code = Some((r.at + 8, code_len));
                }
                r.skip(len)?;
            }
            out.push(Member { flags_at, name, desc, code });
        }
        *at = r.at;
        Ok(out)
    }

    fn utf8(&self, index: u16) -> Result<String, Malformed> {
        let at = *self.cp.get(index as usize).filter(|&&a| a != 0).ok_or(Malformed)?;
        if self.bytes[at] != UTF8 {
            return Err(Malformed);
        }
        let len = u16::from_be_bytes([self.bytes[at + 1], self.bytes[at + 2]]) as usize;
        // Modified UTF-8 matches UTF-8 for every name a patch can spell.
        Ok(String::from_utf8_lossy(&self.bytes[at + 3..at + 3 + len]).into_owned())
    }

    fn u2_at(&self, at: usize) -> u16 {
        u16::from_be_bytes([self.bytes[at], self.bytes[at + 1]])
    }

    fn class_ref(&self, index: u16) -> Result<String, Malformed> {
        let at = *self.cp.get(index as usize).filter(|&&a| a != 0).ok_or(Malformed)?;
        if self.bytes[at] != CLASS {
            return Err(Malformed);
        }
        self.utf8(self.u2_at(at + 1))
    }

    /// The method or interface method reference at `index`.
    fn method_ref(&self, index: u16) -> Result<MemberRef, Malformed> {
        let at = *self.cp.get(index as usize).filter(|&&a| a != 0).ok_or(Malformed)?;
        if !matches!(self.bytes[at], 10 | 11) {
            return Err(Malformed);
        }
        let owner = self.class_ref(self.u2_at(at + 1))?;
        let nat = self.cp[self.u2_at(at + 3) as usize];
        Ok(MemberRef { owner, name: self.utf8(self.u2_at(nat + 1))?, desc: self.utf8(self.u2_at(nat + 3))? })
    }

    fn push(&mut self, entry: &[u8]) -> Result<u16, Malformed> {
        if self.count >= u16::MAX as usize {
            return Err(Malformed);
        }
        self.extra.extend_from_slice(entry);
        self.count += 1;
        Ok(self.count as u16 - 1)
    }

    fn add_utf8(&mut self, s: &str) -> Result<u16, Malformed> {
        if let Some(&i) = self.utf8_index.get(s) {
            return Ok(i);
        }
        let mut e = vec![UTF8];
        e.extend_from_slice(&(s.len() as u16).to_be_bytes());
        e.extend_from_slice(s.as_bytes());
        let i = self.push(&e)?;
        self.utf8_index.insert(s.to_string(), i);
        Ok(i)
    }

    /// Appends a `Methodref` to the static method `owner.name desc`.
    fn add_methodref(&mut self, owner: &str, name: &str, desc: &str) -> Result<u16, Malformed> {
        let (o, n, d) = (self.add_utf8(owner)?, self.add_utf8(name)?, self.add_utf8(desc)?);
        let class = self.push(&[CLASS, (o >> 8) as u8, o as u8])?;
        let nat = self.push(&[NAME_AND_TYPE, (n >> 8) as u8, n as u8, (d >> 8) as u8, d as u8])?;
        self.push(&[METHODREF, (class >> 8) as u8, class as u8, (nat >> 8) as u8, nat as u8])
    }

    /// Makes the class, or its members named `member` (any descriptor), public and not final.
    /// False when no such member exists.
    pub(crate) fn open(&mut self, member: Option<&str>) -> bool {
        let at: Vec<usize> = match member {
            None => vec![self.class_flags_at],
            // Added fields (flags_at 0) are public already.
            Some(m) => self.fields.iter().chain(&self.methods).filter(|x| x.name == m && x.flags_at != 0).map(|x| x.flags_at).collect(),
        };
        for &a in &at {
            let flags = self.u2_at(a) & !(ACC_PRIVATE | ACC_PROTECTED | ACC_FINAL) | ACC_PUBLIC;
            self.bytes[a..a + 2].copy_from_slice(&flags.to_be_bytes());
        }
        !at.is_empty()
    }

    /// In the method `name desc` (every method with code for `name` `*`), turns every call of
    /// `call` into `invokestatic` of `to_owner.to_name`, whose descriptor takes the receiver
    /// first for an instance call. Returns how many calls were redirected (None: no such
    /// method).
    pub(crate) fn redirect(&mut self, name: &str, desc: &str, call: &MemberRef, to_owner: &str, to_name: &str) -> Option<usize> {
        let bodies: Vec<(usize, usize)> = if name == "*" {
            self.methods.iter().filter_map(|m| m.code).collect()
        } else {
            vec![self.methods.iter().find(|m| m.name == name && m.desc == desc)?.code?]
        };
        let mut sites = Vec::new();
        for (start, len) in bodies {
            let mut pc = 0;
            while pc < len {
                let code = &self.bytes[start..start + len];
                let op = code[pc];
                if matches!(op, INVOKEVIRTUAL | INVOKESPECIAL | INVOKESTATIC | INVOKEINTERFACE) {
                    let index = u16::from_be_bytes([code[pc + 1], code[pc + 2]]);
                    if self.method_ref(index).ok().as_ref() == Some(call) {
                        sites.push((start + pc, op));
                    }
                }
                pc += crate::jit::insn_len(code, pc);
            }
        }
        if sites.is_empty() {
            return Some(0);
        }
        let to_desc = static_desc(call, sites.iter().any(|&(_, op)| op != INVOKESTATIC));
        let index = self.add_methodref(to_owner, to_name, &to_desc).ok()?;
        for &(at, op) in &sites {
            self.bytes[at] = INVOKESTATIC;
            self.bytes[at + 1..at + 3].copy_from_slice(&index.to_be_bytes());
            if op == INVOKEINTERFACE {
                self.bytes[at + 3] = NOP;
                self.bytes[at + 4] = NOP;
            }
        }
        Some(sites.len())
    }

    /// Adds a public field `name` of type `desc` (static when `is_static`), initially zero.
    /// False when the class already has a field of that name.
    pub(crate) fn add_field(&mut self, name: &str, desc: &str, is_static: bool) -> bool {
        if self.fields.iter().any(|f| f.name == name) || self.added_count == u16::MAX {
            return false;
        }
        let (Ok(n), Ok(d)) = (self.add_utf8(name), self.add_utf8(desc)) else { return false };
        let flags = ACC_PUBLIC | if is_static { ACC_STATIC } else { 0 };
        for v in [flags, n, d, 0] {
            self.added.extend_from_slice(&v.to_be_bytes());
        }
        self.fields.push(Member { flags_at: 0, name: name.to_string(), desc: desc.to_string(), code: None });
        self.added_count += 1;
        true
    }

    /// The edited class file.
    pub(crate) fn finish(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes.len() + self.extra.len() + self.added.len());
        out.extend_from_slice(&self.bytes[..8]);
        out.extend_from_slice(&(self.count as u16).to_be_bytes());
        out.extend_from_slice(&self.bytes[10..self.cp_end]);
        out.extend_from_slice(&self.extra);
        out.extend_from_slice(&self.bytes[self.cp_end..self.fields_at]);
        out.extend_from_slice(&(self.u2_at(self.fields_at) + self.added_count).to_be_bytes());
        out.extend_from_slice(&self.bytes[self.fields_at + 2..self.fields_end]);
        out.extend_from_slice(&self.added);
        out.extend_from_slice(&self.bytes[self.fields_end..]);
        out
    }
}

/// The descriptor of the static method replacing `call`: the receiver comes first for an
/// instance call.
fn static_desc(call: &MemberRef, instance: bool) -> String {
    if instance {
        format!("(L{};{}", call.owner, &call.desc[1..])
    } else {
        call.desc.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_descriptors() {
        let call = MemberRef { owner: "a/B".into(), name: "f".into(), desc: "(IJ)V".into() };
        assert_eq!(static_desc(&call, true), "(La/B;IJ)V");
        assert_eq!(static_desc(&call, false), "(IJ)V");
    }
}
