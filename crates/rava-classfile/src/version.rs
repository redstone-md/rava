//! Class-file version ↔ Java feature version. Newer Minecraft builds compile
//! against newer Java (e.g. 1.21.x → Java 21, classfile major 65), so the
//! engine must know which versions it accepts and report mismatches clearly.

/// Highest Java feature version Rava intends to execute. Java 22 = major 66.
pub const MAX_SUPPORTED_JAVA: u16 = 22;

/// classfile `major_version` → Java feature version (JVMS 4.1: major 65 = Java 21).
pub fn java_feature(major: u16) -> u16 {
    major.saturating_sub(44)
}

/// classfile major for a given Java feature version (inverse of `java_feature`).
pub fn major_for(java_feature: u16) -> u16 {
    java_feature + 44
}

/// Whether Rava claims support for this class-file major version.
pub fn is_supported(major: u16) -> bool {
    java_feature(major) <= MAX_SUPPORTED_JAVA
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java21_is_major65() {
        assert_eq!(java_feature(65), 21);
        assert_eq!(major_for(21), 65);
        assert!(is_supported(65)); // Java 21
        assert!(is_supported(66)); // Java 22
        assert!(!is_supported(67)); // Java 23 — beyond current target
    }
}
