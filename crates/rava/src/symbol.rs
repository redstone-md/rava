//! Interned names. Every class, member name and descriptor becomes one `Symbol`, so equality is a
//! pointer compare and the text lives once for the life of the process (as in HotSpot's
//! SymbolTable). Symbols are never freed; boot classes never unload.

use std::collections::HashSet;
use std::fmt;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::sync::{Mutex, OnceLock};

/// FxHash (rustc's hasher): a multiply-rotate per word, fine for names and addresses.
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.write_u64(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let mut tail = [0u8; 8];
        tail[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
        self.write_u64(u64::from_le_bytes(tail) ^ bytes.len() as u64);
    }
    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    #[inline]
    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FxBuild = BuildHasherDefault<FxHasher>;

#[derive(Clone, Copy)]
pub struct Symbol(&'static str);

impl Symbol {
    pub fn intern(text: &str) -> Symbol {
        const SHARDS: usize = 32;
        type Shard = Mutex<HashSet<&'static str, BuildHasherDefault<FxHasher>>>;
        static TABLE: OnceLock<Vec<Shard>> = OnceLock::new();
        let shards = TABLE.get_or_init(|| (0..SHARDS).map(|_| Mutex::new(HashSet::default())).collect());
        let mut h = FxHasher::default();
        h.write(text.as_bytes());
        let mut table = shards[h.finish() as usize % SHARDS].lock().unwrap();
        if let Some(&existing) = table.get(text) {
            return Symbol(existing);
        }
        let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
        table.insert(leaked);
        Symbol(leaked)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl PartialEq for Symbol {
    fn eq(&self, other: &Symbol) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Symbol {}

impl Hash for Symbol {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0.as_ptr() as usize).hash(state)
    }
}

impl PartialEq<str> for Symbol {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Symbol {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.0, f)
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Decodes JVM "modified UTF-8" (JVMS 4.4.7) into UTF-16 code units, exactly.
pub fn decode_modified_utf8(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i] as u16;
        if b < 0x80 {
            out.push(b);
            i += 1;
        } else if b & 0xE0 == 0xC0 && i + 1 < bytes.len() {
            out.push(((b & 0x1F) << 6) | (bytes[i + 1] as u16 & 0x3F));
            i += 2;
        } else if i + 2 < bytes.len() {
            out.push(((b & 0x0F) << 12) | ((bytes[i + 1] as u16 & 0x3F) << 6) | (bytes[i + 2] as u16 & 0x3F));
            i += 3;
        } else {
            out.push(0xFFFD);
            i += 1;
        }
    }
    out
}

/// Modified UTF-8 → Rust text for names. Unpaired surrogates (legal in class files, absent from
/// real names) become U+FFFD; string *literals* are decoded exactly with `decode_modified_utf8`.
pub fn modified_utf8_to_string(bytes: &[u8]) -> String {
    if bytes.iter().all(|&b| b < 0x80 && b != 0) {
        // SAFETY: pure ASCII is valid UTF-8.
        return unsafe { std::str::from_utf8_unchecked(bytes) }.to_owned();
    }
    String::from_utf16_lossy(&decode_modified_utf8(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_pointer_identity() {
        let a = Symbol::intern("java/lang/Object");
        let b = Symbol::intern(&String::from("java/lang/Object"));
        assert_eq!(a, b);
        assert!(std::ptr::eq(a.as_str(), b.as_str()));
        assert_ne!(a, Symbol::intern("java/lang/String"));
    }

    #[test]
    fn modified_utf8_decodes_nul_and_surrogates() {
        // U+0000 is C0 80; U+1F600 is a surrogate pair, each half 3 bytes.
        let bytes = [0x41, 0xC0, 0x80, 0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80];
        assert_eq!(decode_modified_utf8(&bytes), vec![0x41, 0x0000, 0xD83D, 0xDE00]);
    }
}
