//! The process-wide VM: class image, heap, loaded classes, interned strings, natives.

use crate::gc::Heap;
use crate::heap::Oop;
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
    /// `java.class.path` for the system class loader.
    pub class_path: String,
    /// `-Dkey=value` properties.
    pub properties: Vec<(String, String)>,
    pub assertions: crate::assertions::Assertions,
    /// `--enable-preview`.
    pub preview: bool,
    pub heap_reserve: usize,
    /// The most the heap grows to (`-Xmx`); by default a quarter of physical memory, as
    /// HotSpot's `MaxRAMPercentage`.
    pub max_heap: usize,
}

impl VmOptions {
    pub fn new(java_home: PathBuf) -> Self {
        VmOptions {
            java_home,
            boot_append: Vec::new(),
            class_path: String::new(),
            properties: Vec::new(),
            assertions: Default::default(),
            preview: false,
            heap_reserve: 64 << 30,
            max_heap: default_max_heap(),
        }
    }
}

fn default_max_heap() -> usize {
    match crate::os::physical_memory() {
        0 => 1 << 30,
        bytes => (bytes as usize / 4).max(128 << 20),
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
    /// Every class ever defined, hidden and array classes included (collector roots).
    pub(crate) all_classes: Mutex<Vec<&'static Klass>>,
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
/// The process VM once it exists (for threads that may start before it, the profiler's).
pub fn created() -> Option<&'static Vm> {
    VM.get()
}

impl Vm {
    /// Creates the process VM (once). Later calls return the existing instance.
    pub fn create(options: VmOptions) -> std::io::Result<&'static Vm> {
        if let Some(vm) = VM.get() {
            return Ok(vm);
        }
        crate::jdk::init(&options.java_home);
        let image = JImage::open_java_home(&options.java_home)?;
        let heap = Heap::new(options.heap_reserve, options.max_heap.min(options.heap_reserve));
        let _ = VM.set(Vm {
            image,
            options,
            heap,
            classes: RwLock::new(HashMap::new()),
            strings: Mutex::new(HashMap::new()),
            all_classes: Mutex::new(Vec::new()),
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
        let k = *self.classes.write().unwrap().entry((loader as usize, k.name)).or_insert(k);
        if loader.is_null() {
            crate::natives::classloader::note_boot_class(&self.image, k.name);
        }
        k
    }

    /// Every class defined so far, in definition order.
    pub(crate) fn all_classes(&self) -> Vec<&'static Klass> {
        self.all_classes.lock().unwrap().clone()
    }

    /// The statics of `classes` (the only class roots that change after being set).
    /// The address of each reference-typed static slot of `classes` (full-width references).
    pub(crate) fn class_statics(&self, classes: &[&'static Klass], slot: &mut dyn FnMut(usize)) {
        for &k in classes {
            for f in k.fields.iter().filter(|f| f.is_static() && f.ty.is_ref()) {
                slot(&k.statics[f.offset as usize] as *const _ as usize);
            }
        }
    }

    /// The roots held by `classes` besides statics: mirrors, loaders and resolved constants.
    pub(crate) fn class_roots(&self, classes: &[&'static Klass], precise: &mut dyn FnMut(Oop)) {
        for &k in classes {
            precise(k.mirror());
            precise(k.loader());
            for r in k.resolved.iter() {
                match r.get() {
                    Some(crate::link::Resolved::String(o)) | Some(crate::link::Resolved::Constant(o)) => precise(o as Oop),
                    _ => {}
                }
            }
        }
    }

    /// The roots held outside classes: interned strings (unless `young`: new ones are noted in
    /// `gc::recent`), primitive mirrors, call sites, starting threads, modules and Flight
    /// Recorder configurations.
    pub(crate) fn other_roots(&self, young: bool, precise: &mut dyn FnMut(Oop)) {
        if !young {
            for &s in self.strings.lock().unwrap().values() {
                precise(s as Oop);
            }
        }
        for m in crate::runtime::primitive_mirrors() {
            precise(m);
        }
        crate::interp::indy::for_each_root(precise);
        crate::natives::thread::for_each_starting(precise);
        crate::natives::module_roots(precise);
        crate::natives::jfr_roots(precise);
        crate::jni::roots(precise);
    }

    pub(crate) fn record_class(&self, k: &'static Klass) {
        self.all_classes.lock().unwrap().push(k);
        crate::jit::note_class(k);
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
