//! macOS preferences in the JDK's CoreFoundation domain/tree format.
//! API and file layout: OpenJDK 21's java.prefs/macosx MacOSXPreferencesFile.m.

mod cf;
use cf::Owned;

pub(crate) type MacPrefsResult<T> = Result<T, ()>;
const SLASH: u16 = b'/' as u16;

pub(crate) fn mac_prefs_constants() -> [u64; 4] { cf::constants() }

/// One exact CFPreferences application/user/host domain; Java serializes its operations.
pub(crate) struct MacPrefs {
    name: Vec<u16>,
    app: Owned,
    user: u64,
    host: u64,
}

impl MacPrefs {
    pub(crate) fn new(name: Vec<u16>, user: u64, host: u64) -> MacPrefsResult<Self> {
        let app = Owned::string(&name)?;
        Ok(Self { name, app, user, host })
    }

    fn tree(&self, path: &[u16]) -> MacPrefsResult<(Owned, Option<Owned>, usize)> {
        let length = root_length(path, &self.name).ok_or(())?;
        let key = Owned::string(&path[..length])?;
        let value = cf::copy(&key, &self.app, self.user, self.host).filter(Owned::is_dictionary);
        Ok((key, value, length))
    }

    fn find(&self, path: &[u16]) -> MacPrefsResult<Option<Owned>> {
        if root_length(path, &self.name).is_none() { return Ok(None) }
        let (_, tree, length) = self.tree(path)?;
        let Some(mut node) = tree else { return Ok(None) };
        for part in path[length..].split_inclusive(|&c| c == SLASH) {
            let key = Owned::string(part)?;
            let Some(child) = node.get(&key).filter(Owned::is_dictionary) else { return Ok(None) };
            node = child;
        }
        Ok(Some(node))
    }

    /// Deep-copy all containers before changing them; CFPreferences returns immutable values.
    fn mutable_node(&self, path: &[u16]) -> MacPrefsResult<(Owned, Owned, Owned)> {
        let (key, old, length) = self.tree(path)?;
        let top = match old { Some(old) => old.mutable_copy()?, None => Owned::dictionary()? };
        let mut node = top.retained();
        for part in path[length..].split_inclusive(|&c| c == SLASH) {
            let part = Owned::string(part)?;
            node = match node.get(&part).filter(Owned::is_dictionary) {
                Some(child) => child,
                None => {
                    let child = Owned::dictionary()?;
                    node.add(&part, &child);
                    child
                }
            };
        }
        Ok((key, top, node))
    }

    fn save(&self, key: &Owned, value: Option<&Owned>) { cf::store(key, value, &self.app, self.user, self.host); }

    pub(crate) fn add_node(&self, path: &[u16]) -> MacPrefsResult<bool> {
        if self.find(path)?.is_some() { return Ok(false) }
        let (key, top, _) = self.mutable_node(path)?;
        self.save(&key, Some(&top));
        Ok(true)
    }

    pub(crate) fn remove_node(&self, path: &[u16]) -> MacPrefsResult<()> {
        let Some((parent, child)) = parent_child(path) else { return Ok(()) };
        // A package-domain root has no parent in this file; delete its preference key.
        if root_length(path, &self.name) == Some(path.len()) {
            let (key, value, _) = self.tree(path)?;
            if value.is_some() { self.save(&key, None); }
            return Ok(())
        }
        let child_key = Owned::string(child)?;
        if self.find(parent)?.is_some_and(|node| node.contains(&child_key)) {
            self.remove(parent, child)?;
        }
        Ok(())
    }

    pub(crate) fn add_child(&self, path: &[u16], child: &[u16]) -> MacPrefsResult<bool> {
        let (key, top, node) = self.mutable_node(path)?;
        let child = Owned::string(child)?;
        let added = !node.contains(&child);
        if added { node.add(&child, &Owned::dictionary()?); }
        self.save(&key, Some(&top));
        Ok(added)
    }

    pub(crate) fn add_key(&self, path: &[u16], key: &[u16], value: &[u16]) -> MacPrefsResult<()> {
        let (top_key, top, node) = self.mutable_node(path)?;
        node.set(&Owned::string(key)?, &Owned::string(value)?);
        self.save(&top_key, Some(&top));
        Ok(())
    }

    /// Removing a missing entry is a no-op, including a missing parent node.
    pub(crate) fn remove(&self, path: &[u16], entry: &[u16]) -> MacPrefsResult<()> {
        let key = Owned::string(entry)?;
        if !self.find(path)?.is_some_and(|node| node.contains(&key)) { return Ok(()) }
        let (top_key, top, node) = self.mutable_node(path)?;
        node.remove(&key);
        self.save(&top_key, Some(&top));
        Ok(())
    }

    pub(crate) fn get_key(&self, path: &[u16], key: &[u16]) -> MacPrefsResult<Option<Vec<u16>>> {
        let Some(node) = self.find(path)? else { return Ok(None) };
        let Some(value) = node.get(&Owned::string(key)?) else { return Ok(None) };
        value.units()
    }

    pub(crate) fn names(&self, path: &[u16], children: bool) -> MacPrefsResult<Vec<Vec<u16>>> {
        let Some(node) = self.find(path)? else { return Ok(Vec::new()) };
        let mut names = Vec::new();
        for key in node.keys() {
            let Some(mut units) = key.units()? else { continue };
            if children != (units.last() == Some(&SLASH)) { continue }
            if children { units.pop(); }
            if !units.is_empty() { names.push(units); }
        }
        Ok(names)
    }

    pub(crate) fn synchronize(&self) -> bool { cf::synchronize(&self.app, self.user, self.host) }
}

fn root_length(path: &[u16], name: &[u16]) -> Option<usize> {
    if path.first() != Some(&SLASH) || path.last() != Some(&SLASH) { return None }
    if name.iter().copied().eq("com.apple.java.util.prefs".encode_utf16()) { return Some(1) }
    path.iter().enumerate().filter(|&(i, &c)| i != 0 && c == SLASH).nth(2).map(|(i, _)| i + 1)
}

fn parent_child(path: &[u16]) -> Option<(&[u16], &[u16])> {
    let last = path.len().checked_sub(1)?;
    let slash = path[..last].iter().rposition(|&c| c == SLASH)?;
    Some((&path[..slash + 1], &path[slash + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn units(s: &str) -> Vec<u16> { s.encode_utf16().collect() }

    #[test]
    fn jdk_tree_roots_and_child_paths() {
        assert_eq!(root_length(&units("/a/b/c/d/"), &units("com.apple.java.util.prefs")), Some(1));
        assert_eq!(root_length(&units("/a/b/c/d/"), &units("a.b.c")), Some(7));
        assert_eq!(root_length(&units("/a/b/"), &units("a.b.c")), None);
        assert_eq!(root_length(&units("/"), &units("com.apple.java.util.prefs")), Some(1));
        assert_eq!(parent_child(&units("/a/b/")), Some((units("/a/").as_slice(), units("b/").as_slice())));
        assert_eq!(parent_child(&units("/")), None);
    }
}
