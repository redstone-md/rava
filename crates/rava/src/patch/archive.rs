//! A patch's files: the entries of a jar (zip, stored or deflated) or of an unpacked
//! directory, read whole at startup.

use std::collections::BTreeMap;
use std::path::Path;

/// File name → contents.
pub(crate) type Entries = BTreeMap<String, Vec<u8>>;

fn u2(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn u4(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// The files of the jar at `path`.
pub(crate) fn read_jar(path: &Path) -> Result<Entries, String> {
    let b = std::fs::read(path).map_err(|e| e.to_string())?;
    let bad = || "not a zip file".to_string();
    // End of central directory: the last `PK\5\6` within the trailing comment's reach.
    let eocd = (0..b.len().saturating_sub(21)).rev().take(65536 + 22).find(|&i| b[i..i + 4] == *b"PK\x05\x06").ok_or_else(bad)?;
    let (count, mut at) = (u2(&b, eocd + 10).ok_or_else(bad)?, u4(&b, eocd + 16).ok_or_else(bad)?);
    let mut out = Entries::new();
    for _ in 0..count {
        if b.get(at..at + 4) != Some(b"PK\x01\x02") {
            return Err(bad());
        }
        let method = u2(&b, at + 10).ok_or_else(bad)?;
        let (size, raw) = (u4(&b, at + 20).ok_or_else(bad)?, u4(&b, at + 24).ok_or_else(bad)?);
        let (n, extra, comment) = (u2(&b, at + 28).ok_or_else(bad)?, u2(&b, at + 30).ok_or_else(bad)?, u2(&b, at + 32).ok_or_else(bad)?);
        let local = u4(&b, at + 42).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(b.get(at + 46..at + 46 + n).ok_or_else(bad)?).into_owned();
        at += 46 + n + extra + comment;
        if name.ends_with('/') {
            continue;
        }
        let start = local + 30 + u2(&b, local + 26).ok_or_else(bad)? + u2(&b, local + 28).ok_or_else(bad)?;
        let data = b.get(start..start + size).ok_or_else(bad)?;
        let data = match method {
            0 => data.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec(data).map_err(|e| format!("{name}: {e:?}"))?,
            m => return Err(format!("{name}: compression method {m}")),
        };
        if data.len() != raw {
            return Err(format!("{name}: size mismatch"));
        }
        out.insert(name, data);
    }
    Ok(out)
}

/// The files under the directory `root`, named relative to it with `/` separators.
pub(crate) fn read_dir(root: &Path) -> Result<Entries, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Entries) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            if p.is_dir() {
                walk(root, &p, out)?;
            } else {
                let rel = p.strip_prefix(root).expect("under root").components();
                let name = rel.map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                out.insert(name, std::fs::read(&p)?);
            }
        }
        Ok(())
    }
    let mut out = Entries::new();
    walk(root, root, &mut out).map_err(|e| e.to_string())?;
    Ok(out)
}
