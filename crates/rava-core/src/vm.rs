//! The process-wide VM: class image, heap, loaded classes, interned strings, natives.

use crate::heap::{Heap, Oop};
use crate::jimage::JImage;
use crate::klass::Klass;
use crate::natives::NativeRegistry;
use crate::symbol::Symbol;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};

pub struct VmOptions {
    pub java_home: PathBuf,
    /// Directories searched by the boot loader after the runtime image (`-Xbootclasspath/a`).
    pub boot_append: Vec<PathBuf>,
    pub heap_reserve: usize,
}

impl Default for VmOptions {
    fn default() -> Self {
        VmOptions {
            java_home: crate::java_home().expect("no JDK found: set JAVA_HOME"),
            boot_append: Vec::new(),
            heap_reserve: 64 << 30,
        }
    }
}

/// Classes the VM itself needs by identity. Filled while bootstrapping.
#[derive(Default)]
pub struct WellKnown {
    pub object: OnceLock<&'static Klass>,
    pub class: OnceLock<&'static Klass>,
    pub string: OnceLock<&'static Klass>,
    pub cloneable: OnceLock<&'static Klass>,
    pub serializable: OnceLock<&'static Klass>,
}

pub struct Vm {
    pub image: JImage,
    pub options: VmOptions,
    pub heap: Heap,
    /// Defined classes, keyed by (defining loader address, name). The boot loader is 0.
    pub(crate) classes: RwLock<HashMap<(usize, Symbol), &'static Klass>>,
    pub(crate) strings: Mutex<HashMap<Box<[u16]>, usize>>,
    pub natives: NativeRegistry,
    pub wk: WellKnown,
}

// Raw object addresses in the tables are owned by the heap; all tables are locked.
unsafe impl Sync for Vm {}
unsafe impl Send for Vm {}

static VM: OnceLock<Vm> = OnceLock::new();

/// The running VM. Panics before `Vm::create`.
#[inline(always)]
pub fn vm() -> &'static Vm {
    VM.get().expect("VM not created")
}

impl Vm {
    /// Creates the process VM (once). Later calls return the existing instance.
    pub fn create(options: VmOptions) -> std::io::Result<&'static Vm> {
        if let Some(vm) = VM.get() {
            return Ok(vm);
        }
        let image = JImage::open_java_home(&options.java_home)?;
        let heap = Heap::new(options.heap_reserve);
        let _ = VM.set(Vm {
            image,
            options,
            heap,
            classes: RwLock::new(HashMap::new()),
            strings: Mutex::new(HashMap::new()),
            natives: NativeRegistry::with_builtins(),
            wk: WellKnown::default(),
        });
        Ok(vm())
    }

    pub fn find_loaded(&self, loader: Oop, name: Symbol) -> Option<&'static Klass> {
        self.classes.read().unwrap().get(&(loader as usize, name)).copied()
    }

    /// Publishes a newly defined class; if another thread won the race, returns its class.
    pub(crate) fn publish(&self, loader: Oop, k: &'static Klass) -> &'static Klass {
        *self.classes.write().unwrap().entry((loader as usize, k.name)).or_insert(k)
    }

    pub fn loaded_class_count(&self) -> usize {
        self.classes.read().unwrap().len()
    }

    /// Class-file bytes for the boot loader: runtime image first, then `boot_append`.
    pub(crate) fn boot_class_bytes(&'static self, name: &str) -> Option<&'static [u8]> {
        if let Some(b) = self.image.class_bytes(name) {
            return Some(b);
        }
        self.options.boot_append.iter().find_map(|dir| {
            let bytes = std::fs::read(dir.join(format!("{name}.class"))).ok()?;
            Some(&*Box::leak(bytes.into_boxed_slice()))
        })
    }
}
