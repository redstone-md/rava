//! Jar access: a `.jar` is a zip whose `META-INF/MANIFEST.MF` names the entry
//! point. This crate encapsulates the archive — callers ask for the main class
//! or for class bytes by binary name and never touch the zip directly.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use zip::ZipArchive;

pub struct Jar {
    archive: ZipArchive<File>,
    names: Vec<String>,
    entries: HashSet<String>,
    sizes: HashMap<String, u64>,
    dirs: HashSet<String>,
}

#[derive(Debug)]
pub enum JarError {
    Io(io::Error),
    Zip(zip::result::ZipError),
    NoManifest,
    NoMainClass,
}

impl std::fmt::Display for JarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JarError::Io(e) => write!(f, "io error: {e}"),
            JarError::Zip(e) => write!(f, "zip error: {e}"),
            JarError::NoManifest => write!(f, "jar has no META-INF/MANIFEST.MF"),
            JarError::NoMainClass => write!(f, "manifest has no Main-Class attribute"),
        }
    }
}

impl std::error::Error for JarError {}

impl From<io::Error> for JarError {
    fn from(e: io::Error) -> Self {
        JarError::Io(e)
    }
}

impl From<zip::result::ZipError> for JarError {
    fn from(e: zip::result::ZipError) -> Self {
        JarError::Zip(e)
    }
}

impl Jar {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JarError> {
        let file = File::open(path)?;
        let mut archive = ZipArchive::new(file)?;
        let mut names = Vec::with_capacity(archive.len());
        let mut entries = HashSet::with_capacity(archive.len());
        let mut sizes = HashMap::with_capacity(archive.len());
        let mut dirs = HashSet::new();
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            let name = file.name().to_string();
            let trimmed = name.trim_end_matches('/');
            if file.is_dir() || name.ends_with('/') {
                dirs.insert(trimmed.to_string());
            } else {
                sizes.insert(name.clone(), file.size());
            }
            add_parent_dirs(trimmed, &mut dirs);
            entries.insert(name.clone());
            names.push(name);
        }
        Ok(Self { archive, names, entries, sizes, dirs })
    }

    /// Total number of entries (classes + resources + directories).
    pub fn entry_count(&self) -> usize {
        self.archive.len()
    }

    /// Number of `.class` entries — a quick health signal for a jar.
    pub fn class_count(&self) -> usize {
        self.names.iter().filter(|n| n.ends_with(".class")).count()
    }

    /// Every archive entry name that starts with `prefix` (directory entries
    /// included). Used to enumerate jar-internal resource trees (e.g. `data/`).
    pub fn entries_under(&self, prefix: &str) -> Vec<String> {
        self.names
            .iter()
            .filter(|n| n.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Whether the archive contains an entry with this exact name.
    pub fn has_entry(&self, name: &str) -> bool {
        self.entries.contains(name)
    }

    /// Whether `name` denotes a directory in the archive resource tree.
    pub fn is_dir(&self, name: &str) -> bool {
        let trimmed = name.trim_end_matches('/');
        trimmed.is_empty() || (self.dirs.contains(trimmed) && !self.has_entry(trimmed))
    }

    /// Uncompressed entry size from the central directory, without reading bytes.
    pub fn entry_size(&self, name: &str) -> Option<u64> {
        self.sizes.get(name).copied()
    }

    /// Read a raw entry by its exact archive path.
    pub fn read_entry(&mut self, name: &str) -> Result<Option<Vec<u8>>, JarError> {
        let mut entry = match self.archive.by_name(name) {
            Ok(e) => e,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf)?;
        Ok(Some(buf))
    }

    /// Read class bytes for a binary name like `net/minecraft/bundler/Main`.
    /// Dotted names (`net.minecraft...`) are normalised to slashes.
    pub fn read_class(&mut self, binary_name: &str) -> Result<Option<Vec<u8>>, JarError> {
        let path = format!("{}.class", binary_name.replace('.', "/"));
        self.read_entry(&path)
    }

    /// The `Main-Class` from the manifest, in binary (slash) form.
    pub fn main_class(&mut self) -> Result<String, JarError> {
        let raw = self
            .read_entry("META-INF/MANIFEST.MF")?
            .ok_or(JarError::NoManifest)?;
        let text = String::from_utf8_lossy(&raw);
        manifest_attribute(&text, "Main-Class")
            .map(|v| v.replace('.', "/"))
            .ok_or(JarError::NoMainClass)
    }
}

fn add_parent_dirs(path: &str, dirs: &mut HashSet<String>) {
    let mut rest = path;
    while let Some((parent, tail)) = rest.rsplit_once('/') {
        if parent.is_empty() || tail.is_empty() {
            break;
        }
        dirs.insert(parent.to_string());
        rest = parent;
    }
}

/// Extract a manifest attribute, honouring the spec's 72-byte line folding
/// where continuation lines begin with a single space.
fn manifest_attribute(manifest: &str, key: &str) -> Option<String> {
    let mut value: Option<String> = None;
    for line in manifest.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(cont) = line.strip_prefix(' ') {
            // Continuation of the attribute we are currently reading.
            if let Some(v) = value.as_mut() {
                v.push_str(cont);
            }
        } else if let Some(rest) = line.strip_prefix(&format!("{key}:")) {
            value = Some(rest.trim().to_string());
        } else if value.is_some() {
            // A new, unrelated attribute — our value is complete.
            break;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{manifest_attribute, Jar};
    use std::io::Write;

    #[test]
    fn reads_simple_attribute() {
        let m = "Manifest-Version: 1.0\r\nMain-Class: net.minecraft.bundler.Main\r\n";
        assert_eq!(
            manifest_attribute(m, "Main-Class").as_deref(),
            Some("net.minecraft.bundler.Main")
        );
    }

    #[test]
    fn folds_continuation_lines() {
        let m = "Main-Class: com.example.Very\r\n LongName\r\n";
        assert_eq!(
            manifest_attribute(m, "Main-Class").as_deref(),
            Some("com.example.VeryLongName")
        );
    }

    #[test]
    fn indexes_entries_sizes_and_implicit_dirs() -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "rava-jar-index-{}-{}.jar",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        {
            let file = std::fs::File::create(&path)?;
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("data/demo/a.json", options)?;
            zip.write_all(b"abc")?;
            zip.start_file("data/demo/b.txt", options)?;
            zip.write_all(b"hello")?;
            zip.finish()?;
        }

        let mut jar = Jar::open(&path)?;
        assert!(jar.has_entry("data/demo/a.json"));
        assert!(!jar.has_entry("data/demo"));
        assert!(jar.is_dir("data"));
        assert!(jar.is_dir("data/demo"));
        assert!(!jar.is_dir("data/demo/a.json"));
        assert_eq!(jar.entry_size("data/demo/a.json"), Some(3));
        assert_eq!(jar.read_entry("data/demo/b.txt")?.as_deref(), Some(&b"hello"[..]));

        let mut entries = jar.entries_under("data/demo/");
        entries.sort();
        assert_eq!(entries, vec!["data/demo/a.json", "data/demo/b.txt"]);

        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
