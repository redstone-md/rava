//! Patches: jars (or unpacked directories) in `rava-patches/` of the working directory that
//! edit the classes a program defines and add classes of their own. rava itself stays
//! program-agnostic; what a patch targets is the patch's business, and nothing changes unless
//! a patch is installed. Format and API: `docs/patches.md`.
//!
//! A patch is a descriptor (`rava-patch.txt`, see `descriptor.rs`) and class files. Its edits
//! are applied when a class loader other than the boot loader defines a target class: all of
//! a class's edits apply, or none do. Its own classes are defined lazily, into whichever
//! loader first resolves one of their names (the loader of the edited code).

mod archive;
mod descriptor;
mod rewrite;

use crate::classfile::ClassFile;
use crate::heap::Oop;
use crate::klass::Klass;
use crate::link::LinkResult;
use crate::symbol::Symbol;
use crate::thread::Thread;
use crate::vm::vm;
use descriptor::{Descriptor, Edit};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

struct Patch {
    id: String,
    version: String,
    /// Target class → edits.
    edits: HashMap<String, Vec<Edit>>,
}

#[derive(Default)]
struct Patches {
    list: Vec<Patch>,
    /// The patches' own classes: name → (patch index, class file).
    classes: HashMap<String, (usize, &'static [u8])>,
    /// Where target classes are written before (`.orig.class`) and after their edits
    /// (`RAVA_PATCH_DUMP`).
    dump: Option<PathBuf>,
}

static PATCHES: OnceLock<Patches> = OnceLock::new();

fn patches() -> &'static Patches {
    PATCHES.get_or_init(Patches::default)
}

/// The directory patches are read from: `RAVA_PATCHES` if set (empty or `0` disables them),
/// else `rava-patches` in the working directory.
fn directory() -> Option<PathBuf> {
    match std::env::var_os("RAVA_PATCHES") {
        Some(v) if v.is_empty() || v == "0" => None,
        Some(v) => Some(PathBuf::from(v)),
        None => Some(PathBuf::from("rava-patches")),
    }
}

/// Reads the installed patches; called once at startup, before any class is defined.
pub(crate) fn discover() {
    let mut p = Patches { dump: std::env::var_os("RAVA_PATCH_DUMP").map(PathBuf::from), ..Patches::default() };
    let Some(dir) = directory() else { return };
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(d) => d.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => return,
    };
    paths.sort();
    for path in paths {
        let entries = if path.is_dir() {
            archive::read_dir(&path)
        } else if path.extension().is_some_and(|e| e == "jar") {
            archive::read_jar(&path)
        } else {
            continue;
        };
        match entries.and_then(|e| load(&mut p, e)) {
            Ok(i) => eprintln!("[rava] patch {} {} from {}", p.list[i].id, p.list[i].version, path.display()),
            Err(e) => eprintln!("[rava] patch {} not loaded: {e}", path.display()),
        }
    }
    let _ = PATCHES.set(p);
}

/// Adds the patch made of `entries`; returns its index.
fn load(p: &mut Patches, mut entries: archive::Entries) -> Result<usize, String> {
    let text = entries.remove("rava-patch.txt").ok_or("no rava-patch.txt")?;
    let d = Descriptor::parse(&String::from_utf8_lossy(&text))?;
    if p.list.iter().any(|q| q.id == d.id) {
        return Err(format!("a patch with id {} is already loaded", d.id));
    }
    let index = p.list.len();
    let mut edits: HashMap<String, Vec<Edit>> = HashMap::new();
    for (class, edit) in d.edits {
        edits.entry(class).or_default().push(edit);
    }
    for (name, bytes) in entries {
        let Some(stem) = name.strip_suffix(".class") else { continue };
        if stem.ends_with("module-info") || stem.starts_with("META-INF/") {
            continue;
        }
        let class = rewrite::class_name(&bytes).ok_or_else(|| format!("{name}: malformed class file"))?;
        if class != stem || p.classes.contains_key(&class) {
            return Err(format!("{name}: misplaced or duplicate class {class}"));
        }
        p.classes.insert(class, (index, Box::leak(bytes.into_boxed_slice())));
    }
    p.list.push(Patch { id: d.id, version: d.version, edits });
    Ok(index)
}

/// Applies one edit; false when its target is missing.
fn apply(c: &mut rewrite::ClassBytes, edit: &Edit) -> bool {
    match edit {
        Edit::Open(member) => c.open(member.as_deref()),
        Edit::Redirect { method, desc, call, to_owner, to_name } => c.redirect(method, desc, call, to_owner, to_name).is_some_and(|n| n > 0),
    }
}

/// The class file a loader other than the boot loader defines for `bytes`: edited by every
/// patch targeting it.
pub(crate) fn transform(bytes: Vec<u8>) -> Vec<u8> {
    let p = patches();
    if p.list.is_empty() {
        return bytes;
    }
    let Some(name) = rewrite::class_name(&bytes) else { return bytes };
    if !p.list.iter().any(|q| q.edits.contains_key(&name)) {
        return bytes;
    }
    if let Some(dir) = &p.dump {
        dump(dir, &format!("{name}.orig"), &bytes);
    }
    let mut out = bytes;
    for patch in &p.list {
        let Some(edits) = patch.edits.get(&name) else { continue };
        let Ok(mut c) = rewrite::ClassBytes::parse(out.clone()) else { continue };
        match edits.iter().position(|e| !apply(&mut c, e)) {
            None => {
                eprintln!("[rava] patch {}: edited {name}", patch.id);
                out = c.finish();
            }
            Some(i) => eprintln!("[rava] patch {}: {name} left unchanged, edit {} has no target: {:?}", patch.id, i + 1, edits[i]),
        }
    }
    if let Some(dir) = &p.dump {
        dump(dir, &name, &out);
    }
    out
}

fn dump(dir: &Path, name: &str, bytes: &[u8]) {
    let path = dir.join(format!("{name}.class"));
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, bytes));
    if let Err(e) = written {
        eprintln!("[rava] patch dump {}: {e}", path.display());
    }
}

/// Defines the patch class `name` for `loader` when one exists (`load_class`, before asking
/// the loader itself).
pub(crate) fn define_own(t: &mut Thread, loader: Oop, name: Symbol) -> Option<LinkResult<&'static Klass>> {
    let &(_, bytes) = patches().classes.get(name.as_str())?;
    let cf = match ClassFile::parse(bytes) {
        Ok(cf) => cf,
        Err(e) => return Some(Err(crate::loader::LoadError { kind: "java/lang/ClassFormatError", message: format!("{name}: {}", e.0) }.into())),
    };
    Some(vm().define_class(t, cf, loader))
}
