//! Explicit offline objects, separate from the writable execution cache.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
pub use super::prepared_archive::{ArchiveWriter, FILE_NAME};

pub(crate) fn read(key: &str) -> std::io::Result<Vec<u8>> {
    static ARCHIVE: OnceLock<Option<super::prepared_archive::Archive>> = OnceLock::new();
    let directory = directory().expect("prepared objects are not enabled");
    let archive = ARCHIVE.get_or_init(|| {
        match super::prepared_archive::Archive::open(&directory.join(FILE_NAME)) {
            Ok(archive) => Some(archive),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("cannot open prepared object container: {error}"),
        }
    });
    match archive {
        Some(archive) => archive.read(key),
        None => std::fs::read(directory.join(key)),
    }
}

pub fn identity() -> Option<&'static str> {
    super::cache_key::compiler_identity()
}

pub fn string_constants() -> bool {
    std::env::var_os("RAVA_AOT_UNSAFE").is_some()
}

fn validate_configuration(path: &Path, strings: bool) -> std::io::Result<()> {
    let expected = if strings { "string_constants=1" } else { "string_constants=0" };
    match std::fs::read_to_string(path.join("configuration")) {
        Ok(actual) if actual.trim() == expected => Ok(()),
        // Legacy builders only emitted the default constant policy.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !strings => Ok(()),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
            "prepared AOT configuration mismatch; prepare with the same constant policy")),
    }
}

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_AOT_PREPARED_DIR").is_some())
}

/// Validate the completed build before accepting any executable object.
pub fn directory() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os("RAVA_AOT_PREPARED_DIR")?);
        let expected = identity().expect("cannot identify AOT compiler executable");
        let stamp = std::fs::read_to_string(path.join("complete"))
            .expect("prepared AOT directory has no completed-build marker");
        assert_eq!(stamp.trim(), expected, "prepared AOT compiler identity mismatch");
        validate_configuration(&path, string_constants()).expect("invalid prepared AOT configuration");
        Some(path)
    }).as_deref()
}
