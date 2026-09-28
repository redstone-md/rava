//! Indexed offline code storage. Contains no guest heap or runtime state.
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

const MAGIC: &[u8; 8] = b"RAVAPAK1";
pub const FILE_NAME: &str = "objects.pack";

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid prepared object container")
}

pub struct ArchiveWriter {
    file: File,
    entries: HashMap<String, (u64, u64)>,
}

impl ArchiveWriter {
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut file = File::create_new(path)?;
        file.write_all(MAGIC)?;
        Ok(Self { file, entries: HashMap::new() })
    }

    pub fn insert(&mut self, key: &str, bytes: &[u8]) -> io::Result<()> {
        u16::try_from(key.len()).map_err(|_| invalid())?;
        let offset = self.file.stream_position()?;
        self.file.write_all(bytes)?;
        // Matches loose-file replacement if two method identities coincide.
        self.entries.insert(key.to_owned(), (offset, bytes.len() as u64));
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let index = self.file.stream_position()?;
        let mut entries: Vec<_> = self.entries.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, (offset, length)) in entries {
            self.file.write_all(&(key.len() as u16).to_le_bytes())?;
            self.file.write_all(key.as_bytes())?;
            self.file.write_all(&offset.to_le_bytes())?;
            self.file.write_all(&length.to_le_bytes())?;
        }
        self.file.write_all(&index.to_le_bytes())?;
        self.file.sync_all()
    }
}

pub struct Archive {
    file: Mutex<File>,
    entries: HashMap<String, (u64, usize)>,
}

impl Archive {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        if size < 16 { return Err(invalid()); }
        let mut word = [0; 8];
        file.read_exact(&mut word)?;
        if &word != MAGIC { return Err(invalid()); }
        file.seek(SeekFrom::End(-8))?;
        file.read_exact(&mut word)?;
        let index = u64::from_le_bytes(word);
        if index < 8 || index > size - 8 { return Err(invalid()); }
        file.seek(SeekFrom::Start(index))?;
        let mut bytes = Vec::new();
        (&mut file).take(size - 8 - index).read_to_end(&mut bytes)?;
        let mut remaining = bytes.as_slice();
        let mut entries = HashMap::new();
        while !remaining.is_empty() {
            let mut key_length = [0; 2];
            remaining.read_exact(&mut key_length)?;
            let n = u16::from_le_bytes(key_length) as usize;
            if remaining.len() < n { return Err(invalid()); }
            let key = std::str::from_utf8(&remaining[..n]).map_err(|_| invalid())?.to_owned();
            remaining = &remaining[n..];
            remaining.read_exact(&mut word)?;
            let offset = u64::from_le_bytes(word);
            remaining.read_exact(&mut word)?;
            let length = u64::from_le_bytes(word);
            if offset < 8 || offset.checked_add(length).is_none_or(|end| end > index) {
                return Err(invalid());
            }
            let length = usize::try_from(length).map_err(|_| invalid())?;
            if entries.insert(key, (offset, length)).is_some() { return Err(invalid()); }
        }
        Ok(Self { file: Mutex::new(file), entries })
    }

    pub fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        let &(offset, length) = self.entries.get(key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unprepared method"))?;
        let mut bytes = vec![0; length];
        let mut file = self.file.lock().map_err(|_| invalid())?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aot_archive_roundtrip_and_corruption() {
        let path = std::env::temp_dir().join(format!("rava-archive-{}-{}.pack",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut writer = ArchiveWriter::create(&path).unwrap();
        writer.insert("one", b"old").unwrap();
        writer.insert("two", b"second").unwrap();
        writer.insert("one", b"new").unwrap();
        writer.finish().unwrap();
        let original = std::fs::read(&path).unwrap();
        {
            let archive = Archive::open(&path).unwrap();
            assert_eq!(archive.read("two").unwrap(), b"second");
            assert_eq!(archive.read("one").unwrap(), b"new");
            assert_eq!(archive.read("absent").unwrap_err().kind(), io::ErrorKind::NotFound);
        }
        assert_eq!(std::fs::read(&path).unwrap(), original);
        for length in [0, 7, 15, original.len() - 1] {
            std::fs::write(&path, &original[..length]).unwrap();
            assert!(Archive::open(&path).is_err());
        }
        let mut corrupt = original.clone();
        let footer = corrupt.len() - 8;
        let index = u64::from_le_bytes(corrupt[footer..].try_into().unwrap()) as usize;
        // First key is "one": corrupt its object's offset, not its contents.
        corrupt[index + 5..index + 13].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&path, corrupt).unwrap();
        assert!(Archive::open(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
