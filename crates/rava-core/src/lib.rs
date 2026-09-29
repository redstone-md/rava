//! rava-core: the parity execution core. Design: `docs/core/README.md`.

pub mod jimage;

use std::path::PathBuf;

/// Locates the JDK whose `lib/modules` supplies the class library: `JAVA_HOME`, else the
/// installation that owns the `java` launcher on `PATH`.
pub fn java_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JAVA_HOME").map(PathBuf::from) {
        if home.join("lib/modules").is_file() {
            return Some(home);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("java"))
        .find(|java| java.is_file())
        .and_then(|java| std::fs::canonicalize(java).ok())
        .and_then(|java| java.parent()?.parent().map(PathBuf::from))
        .filter(|home| home.join("lib/modules").is_file())
}
