//! Reader for the JDK runtime image (`$JAVA_HOME/lib/modules`, format "jimage" 1.0).
//!
//! Layout (header fields little-endian u32):
//! `magic version flags resource_count table_length locations_size strings_size`, then the
//! redirect table (`i32 x table_length`), the location offsets (`u32 x table_length`), the
//! location attribute stream, the string table, and finally resource bytes.
//! Names hash with the FNV-style `ImageStringsReader.hashCode`.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

const MAGIC: u32 = 0xCAFE_DADA;
const HEADER_SIZE: usize = 28;
const HASH_MULTIPLIER: u32 = 0x0100_0193;

const ATTR_END: u8 = 0;
const ATTR_MODULE: usize = 1;
const ATTR_PARENT: usize = 2;
const ATTR_BASE: usize = 3;
const ATTR_EXTENSION: usize = 4;
const ATTR_OFFSET: usize = 5;
const ATTR_COMPRESSED: usize = 6;
const ATTR_UNCOMPRESSED: usize = 7;

/// A read-only memory map of the whole image file.
struct Mapping {
    ptr: *const u8,
    len: usize,
}

// The mapping is immutable for its whole lifetime.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len() as usize;
        // SAFETY: a private read-only mapping of a file we keep no writer to.
        let ptr = unsafe {
            libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0)
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { ptr: ptr as *const u8, len })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: ptr/len describe a live mapping owned by self.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what `open` mapped.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

/// An opened jimage. Lookups are lock-free and allocation-free on the hit path.
pub struct JImage {
    map: Mapping,
    table_length: usize,
    redirect_at: usize,
    offsets_at: usize,
    locations_at: usize,
    strings_at: usize,
    strings_size: usize,
    resources_at: usize,
}

impl JImage {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let map = Mapping::open(path.as_ref())?;
        let b = map.bytes();
        if b.len() < HEADER_SIZE || le32(b, 0) != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a jimage file"));
        }
        let table_length = le32(b, 16) as usize;
        let locations_size = le32(b, 20) as usize;
        let strings_size = le32(b, 24) as usize;
        let redirect_at = HEADER_SIZE;
        let offsets_at = redirect_at + table_length * 4;
        let locations_at = offsets_at + table_length * 4;
        let strings_at = locations_at + locations_size;
        let resources_at = strings_at + strings_size;
        if resources_at > b.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated jimage index"));
        }
        Ok(Self { map, table_length, redirect_at, offsets_at, locations_at, strings_at, strings_size, resources_at })
    }

    /// Opens `lib/modules` of the JDK at `java_home`.
    pub fn open_java_home(java_home: impl AsRef<Path>) -> io::Result<Self> {
        Self::open(java_home.as_ref().join("lib").join("modules"))
    }

    /// Returns the bytes of the resource `/module/parent/base.extension`, e.g.
    /// `/java.base/java/lang/Object.class`.
    pub fn resource(&self, full_name: &str) -> Option<&[u8]> {
        let attrs = self.locate(full_name)?;
        if !self.name_matches(&attrs, full_name) {
            return None;
        }
        if attrs[ATTR_COMPRESSED] != 0 {
            return None; // compressed images are not produced by standard JDK builds
        }
        let start = self.resources_at + attrs[ATTR_OFFSET] as usize;
        self.map.bytes().get(start..start + attrs[ATTR_UNCOMPRESSED] as usize)
    }

    /// The module that owns `package` (internal form, `java/util`), if any.
    pub fn module_of_package(&self, package: &str) -> Option<&str> {
        let dotted = package.replace('/', ".");
        let content = self.resource(&format!("/packages/{dotted}"))?;
        content
            .chunks_exact(8)
            .find(|pair| le32(pair, 0) == 0)
            .and_then(|pair| self.string(le32(pair, 4) as usize))
    }

    /// Bytes of the class `internal_name` (e.g. `java/lang/Object`) in whichever module owns it.
    pub fn class_bytes(&self, internal_name: &str) -> Option<&[u8]> {
        let package = internal_name.rsplit_once('/').map_or("", |(p, _)| p);
        let module = self.module_of_package(package)?;
        self.resource(&format!("/{module}/{internal_name}.class"))
    }

    fn locate(&self, name: &str) -> Option<[u64; 8]> {
        let b = self.map.bytes();
        let slot = hash(name, HASH_MULTIPLIER) as usize % self.table_length;
        let redirect = le32(b, self.redirect_at + slot * 4) as i32;
        let index = match redirect {
            0 => return None,
            r if r < 0 => (-1 - r) as usize,
            r => hash(name, r as u32) as usize % self.table_length,
        };
        let mut pos = self.locations_at + le32(b, self.offsets_at + index * 4) as usize;
        let mut attrs = [0u64; 8];
        loop {
            let head = *b.get(pos)?;
            let kind = head >> 3;
            if kind == ATTR_END {
                return Some(attrs);
            }
            let len = (head & 7) as usize + 1;
            let value = b.get(pos + 1..pos + 1 + len)?.iter().fold(0u64, |v, &x| (v << 8) | x as u64);
            *attrs.get_mut(kind as usize)? = value;
            pos += 1 + len;
        }
    }

    /// A hash hit is only a candidate: confirm every name component.
    fn name_matches(&self, attrs: &[u64; 8], full_name: &str) -> bool {
        let mut rebuilt = String::with_capacity(full_name.len());
        let module = self.string(attrs[ATTR_MODULE] as usize).unwrap_or("");
        if !module.is_empty() {
            rebuilt.push('/');
            rebuilt.push_str(module);
            rebuilt.push('/');
        }
        let parent = self.string(attrs[ATTR_PARENT] as usize).unwrap_or("");
        if !parent.is_empty() {
            rebuilt.push_str(parent);
            rebuilt.push('/');
        }
        rebuilt.push_str(self.string(attrs[ATTR_BASE] as usize).unwrap_or(""));
        let ext = self.string(attrs[ATTR_EXTENSION] as usize).unwrap_or("");
        if !ext.is_empty() {
            rebuilt.push('.');
            rebuilt.push_str(ext);
        }
        rebuilt == full_name
    }

    fn string(&self, offset: usize) -> Option<&str> {
        if offset >= self.strings_size {
            return None;
        }
        let tail = &self.map.bytes()[self.strings_at + offset..self.strings_at + self.strings_size];
        let end = tail.iter().position(|&c| c == 0)?;
        std::str::from_utf8(&tail[..end]).ok()
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn hash(name: &str, seed: u32) -> u32 {
    name.bytes().fold(seed, |h, c| h.wrapping_mul(HASH_MULTIPLIER) ^ c as u32) & 0x7FFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Option<JImage> {
        let home = crate::java_home()?;
        JImage::open_java_home(home).ok()
    }

    #[test]
    fn reads_object_class_from_java_base() {
        let Some(img) = image() else { return };
        let bytes = img.class_bytes("java/lang/Object").expect("Object.class");
        assert_eq!(&bytes[..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(img.module_of_package("java/util"), Some("java.base"));
        assert!(img.class_bytes("java/lang/NoSuchClassAnywhere").is_none());
    }
}
