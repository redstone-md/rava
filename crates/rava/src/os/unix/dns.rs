//! The Unix resolver's hostname-derived fallback domain (OpenJDK's libnet behavior).

/// When resolv.conf has neither a search nor a domain directive, the JDK uses the suffix
/// after the first dot in gethostname's result. Keep its bytes for JNI's modified UTF-8.
pub(crate) fn fallback_domain() -> Option<Vec<u8>> {
    let mut hostname = [0u8; 1025];
    // SAFETY: gethostname receives the entire writable buffer.
    if unsafe { libc::gethostname(hostname.as_mut_ptr().cast(), hostname.len()) } != 0 {
        return None;
    }
    // Some Unix implementations omit NUL when the supplied buffer is too short.
    hostname[1024] = 0;
    hostname_suffix(&hostname).map(<[u8]>::to_vec)
}

fn hostname_suffix(hostname: &[u8]) -> Option<&[u8]> {
    let end = hostname.iter().position(|&b| b == 0).unwrap_or(hostname.len());
    let hostname = &hostname[..end];
    let dot = hostname.iter().position(|&b| b == b'.')?;
    Some(&hostname[dot + 1..])
}

#[cfg(test)]
mod tests {
    use super::hostname_suffix;

    #[test]
    fn fallback_uses_first_dot_and_respects_nul() {
        assert_eq!(hostname_suffix(b"worker.example.test\0"), Some(b"example.test".as_slice()));
        assert_eq!(hostname_suffix(b"worker\0ignored.example"), None);
        assert_eq!(hostname_suffix(b"worker."), Some(b"".as_slice()));
        assert_eq!(hostname_suffix(b".example"), Some(b"example".as_slice()));
        assert_eq!(hostname_suffix(b"worker"), None);
        assert_eq!(hostname_suffix(b""), None);
    }
}
