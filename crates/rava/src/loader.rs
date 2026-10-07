//! Boot class loading: find bytes, parse, lay out fields, build methods and the vtable.

use crate::classfile::ClassFile;
use crate::heap::{Oop, HEADER_GAP, HEADER_SIZE};
use crate::klass::{FieldDesc, Klass, KlassKind, KlassParts, ACC_ABSTRACT, ACC_FINAL, ACC_PUBLIC};
use crate::method::Method;
use crate::symbol::Symbol;
use crate::types::BasicType;
use crate::link::{LinkError, LinkResult};
use crate::runtime as rt;
use crate::thread::{JResult, Thread};
use crate::vm::{vm, Vm};

#[derive(Debug, Clone)]
pub struct LoadError {
    /// `java/lang/NoClassDefFoundError`, `java/lang/ClassFormatError`, ...
    pub kind: &'static str,
    pub message: String,
}

impl LoadError {
    pub(crate) fn no_class(name: &str) -> Self {
        LoadError { kind: "java/lang/NoClassDefFoundError", message: name.replace('/', ".") }
    }
}

pub type LoadResult = Result<&'static Klass, LoadError>;

impl From<LoadError> for LinkError {
    fn from(e: LoadError) -> Self {
        LinkError::Load(e)
    }
}

/// Fields the VM adds to a JDK class's layout, invisible to reflection (as HotSpot's injected
/// fields). Keyed by JDK class names: these are VM structure, not workload routing.
fn injected_fields(class: &str) -> &'static [(&'static str, &'static str)] {
    match class {
        "java/lang/Class" => &[("klass", "J"), ("array_klass", "J"), ("primitive_type", "I"), ("protection_domain", "Ljava/security/ProtectionDomain;")],
        "java/lang/invoke/ResolvedMethodName" => &[("vmtarget", "J"), ("vmholder", "Ljava/lang/Class;")],
        "java/lang/invoke/MemberName" => &[("vmindex", "J")],
        _ => &[],
    }
}

impl Vm {
    /// Loads `name` (internal form or array descriptor) through the boot loader.
    pub fn load_boot_class(&'static self, name: Symbol) -> LoadResult {
        if let Some(k) = self.find_loaded(std::ptr::null_mut(), name) {
            return Ok(k);
        }
        if name.as_str().starts_with('[') {
            return self.array_class_by_descriptor(name);
        }
        let bytes = self.boot_class_bytes(name.as_str()).ok_or_else(|| LoadError::no_class(name.as_str()))?;
        let cf = ClassFile::parse(bytes)
            .map_err(|e| LoadError { kind: "java/lang/ClassFormatError", message: format!("{name}: {}", e.0) })?;
        if cf.name != name {
            return Err(LoadError::no_class(&format!("{name} (wrong name: {})", cf.name)));
        }
        self.define_with(cf, std::ptr::null_mut(), None, &mut |n| self.load_boot_class(n).map_err(LinkError::from))
            .map_err(|e| match e {
                LinkError::Load(e) => e,
                other => LoadError { kind: "java/lang/LinkageError", message: format!("{other:?}") },
            })
    }

    /// Loads `name` as seen from `loader` (JVMS 5.3): the boot loader directly, any other loader
    /// through its Java `loadClass`, recording the loader as an initiating loader.
    pub fn load_class(&'static self, t: &mut Thread, loader: Oop, name: Symbol) -> LinkResult<&'static Klass> {
        if loader.is_null() {
            return Ok(self.load_boot_class(name)?);
        }
        if let Some(k) = self.find_loaded(loader, name) {
            return Ok(k);
        }
        if let Some(comp) = name.as_str().strip_prefix('[') {
            let elem = match comp.as_bytes()[0] {
                b'L' => self.load_class(t, loader, Symbol::intern(&comp[1..comp.len() - 1]))?,
                b'[' => self.load_class(t, loader, Symbol::intern(comp))?,
                _ => return Ok(self.load_boot_class(name)?),
            };
            return Ok(self.array_class_of(elem)?);
        }
        let k = self.call_load_class(t, loader, name).map_err(LinkError::Thrown)?;
        if k.name != name {
            return Err(LoadError::no_class(name.as_str()).into());
        }
        self.classes.write().unwrap().entry((loader as usize, name)).or_insert(k);
        Ok(k)
    }

    fn call_load_class(&'static self, t: &mut Thread, loader: Oop, name: Symbol) -> JResult<&'static Klass> {
        let class_loader = crate::loader::load("java/lang/ClassLoader").expect("ClassLoader");
        let load = class_loader
            .declared_method(Symbol::intern("loadClass"), Symbol::intern("(Ljava/lang/String;)Ljava/lang/Class;"))
            .expect("ClassLoader.loadClass");
        // SAFETY: live loader object.
        let target = crate::link::select_method(unsafe { crate::heap::obj::klass(loader) }, load).unwrap_or(load);
        let jname = rt::new_string(t, &name.as_str().replace('/', "."))?;
        let result = t.call(target, &[loader as u64, jname as u64]);
        let cnfe = crate::loader::load("java/lang/ClassNotFoundException").expect("CNFE");
        let mirror = match result {
            Ok(m) if m != 0 => m as Oop,
            Ok(_) => return Err(rt::throw_new(t, "java/lang/NoClassDefFoundError", &name.as_str().replace('/', "."))),
            // SAFETY: live exception.
            Err(ex) if unsafe { crate::heap::obj::klass(ex.0) }.is_subtype_of(cnfe) => {
                return Err(rt::throw_new(t, "java/lang/NoClassDefFoundError", name.as_str()));
            }
            Err(ex) => return Err(ex),
        };
        Ok(rt::klass_of_mirror(mirror).expect("loadClass returned a primitive class"))
    }

    /// Defines a parsed class for `loader`, resolving its supertypes through that loader.
    pub fn define_class(&'static self, t: &mut Thread, cf: ClassFile, loader: Oop) -> LinkResult<&'static Klass> {
        self.define_with(cf, loader, None, &mut |name| self.load_class(t, loader, name))
    }

    /// Defines a hidden class (`Lookup.defineHiddenClass`): it gets a unique name, is never
    /// registered by name, and its nest host is `nest_host` when it is a nestmate.
    pub fn define_hidden_class(
        &'static self,
        t: &mut Thread,
        cf: ClassFile,
        loader: Oop,
        nest_host: Option<&'static Klass>,
    ) -> LinkResult<&'static Klass> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x0800_0000);
        let id = NEXT.fetch_add(0x400, std::sync::atomic::Ordering::Relaxed);
        let name = Symbol::intern(&format!("{}/0x{id:016x}", cf.name));
        let k = self.define_with(cf, loader, Some(name), &mut |n| self.load_class(t, loader, n))?;
        if let Some(h) = nest_host {
            let _ = k.nest_host.set(h);
        }
        Ok(k)
    }

    fn define_with(
        &'static self,
        cf: ClassFile,
        loader: Oop,
        hidden_name: Option<Symbol>,
        resolve: &mut dyn FnMut(Symbol) -> LinkResult<&'static Klass>,
    ) -> LinkResult<&'static Klass> {
        let super_klass = match cf.super_name {
            Some(s) => Some(resolve(s)?),
            None => None,
        };
        if super_klass.is_some_and(|s| s.is_interface()) {
            return Err(LinkError::Error("java/lang/IncompatibleClassChangeError", format!("class {} has interface as super class", cf.name)));
        }
        let interfaces = cf.interfaces.iter().map(|&i| resolve(i)).collect::<Result<Vec<_>, _>>()?;
        let layout = FieldLayout::compute(&cf, super_klass);
        let klass: &'static Klass = crate::klass::leak(Klass::new(KlassParts {
            name: hidden_name.unwrap_or(cf.name),
            kind: KlassKind::Instance,
            access: cf.access,
            super_klass,
            interfaces,
            fields: layout.fields,
            instance_size: layout.instance_size,
            ref_offsets: layout.ref_offsets,
            static_slots: layout.static_slots,
            initialized: false,
            hidden: hidden_name.is_some(),
            cf: Some(cf),
        }));
        klass.loader.store(loader, std::sync::atomic::Ordering::Release);
        self.record_class(klass);
        let cf = klass.cf.as_ref().unwrap();
        let methods: Vec<Method> =
            cf.methods.iter().map(|m| {
                let stub = loader.is_null() && self.natives.has_stub(klass.name.as_str(), m.name.as_str(), m.desc.as_str());
                Method::new(klass, m.name, m.desc, m.access, m.code.as_ref(), stub)
            }).collect();
        let _ = klass.methods.set(crate::low::leak_slice(methods));
        klass.set_vtable(build_vtable(klass));
        if hidden_name.is_some() {
            return Ok(klass);
        }
        self.register_well_known(klass);
        Ok(self.publish(loader, klass))
    }

    /// The array class `[elem` for any element class.
    pub fn array_class_of(&'static self, elem: &'static Klass) -> LoadResult {
        if let Some(k) = elem.array_klass.get() {
            return Ok(k);
        }
        let desc = if elem.is_array() { format!("[{}", elem.name) } else { format!("[L{};", elem.name) };
        let k = self.new_array_klass(Symbol::intern(&desc), KlassKind::ObjArray { elem }, elem.loader())?;
        Ok(*elem.array_klass.get_or_init(|| k))
    }

    pub fn array_class_by_descriptor(&'static self, desc: Symbol) -> LoadResult {
        let comp = &desc.as_str()[1..];
        match comp.as_bytes()[0] {
            b'L' => self.array_class_of(self.load_boot_class(Symbol::intern(&comp[1..comp.len() - 1]))?),
            b'[' => self.array_class_of(self.load_boot_class(Symbol::intern(comp))?),
            c => self.primitive_array_class(BasicType::from_desc(c)),
        }
    }

    pub fn primitive_array_class(&'static self, elem: BasicType) -> LoadResult {
        // Asked for every string and byte array the VM makes: one cell per element type instead
        // of formatting, interning and looking up the name each time.
        static CLASSES: [std::sync::OnceLock<&'static Klass>; 10] = [const { std::sync::OnceLock::new() }; 10];
        let cell = &CLASSES[elem as usize];
        if let Some(&k) = cell.get() {
            return Ok(k);
        }
        let name = Symbol::intern(&format!("[{}", elem.desc_char()));
        let k = match self.find_loaded(std::ptr::null_mut(), name) {
            Some(k) => k,
            None => self.new_array_klass(name, KlassKind::TypeArray { elem }, std::ptr::null_mut())?,
        };
        Ok(*cell.get_or_init(|| k))
    }

    fn new_array_klass(&'static self, name: Symbol, kind: KlassKind, loader: Oop) -> LoadResult {
        let object = self.load_boot_class(Symbol::intern("java/lang/Object"))?;
        let interfaces = vec![
            self.load_boot_class(Symbol::intern("java/lang/Cloneable"))?,
            self.load_boot_class(Symbol::intern("java/io/Serializable"))?,
        ];
        let klass: &'static Klass = crate::klass::leak(Klass::new(KlassParts {
            name,
            kind,
            access: ACC_PUBLIC | ACC_FINAL | ACC_ABSTRACT,
            super_klass: Some(object),
            interfaces,
            cf: None,
            fields: Vec::new(),
            instance_size: 0,
            ref_offsets: Vec::new(),
            static_slots: 0,
            initialized: true,
            hidden: false,
        }));
        klass.loader.store(loader, std::sync::atomic::Ordering::Release);
        self.record_class(klass);
        let _ = klass.methods.set(&[]);
        klass.set_vtable(object.vtable().to_vec().into_boxed_slice());
        Ok(self.publish(loader, klass))
    }

    fn register_well_known(&self, k: &'static Klass) {
        let wk = &self.wk;
        let slot = match k.name.as_str() {
            "java/lang/Object" => &wk.object,
            "java/lang/Class" => &wk.class,
            "java/lang/String" => &wk.string,
            "java/lang/Cloneable" => &wk.cloneable,
            "java/io/Serializable" => &wk.serializable,
            _ => return,
        };
        let _ = slot.set(k);
    }
}

struct FieldLayout {
    fields: Vec<FieldDesc>,
    instance_size: u32,
    ref_offsets: Vec<u32>,
    static_slots: usize,
}

impl FieldLayout {
    /// Instance fields pack after the superclass's, largest first so every field is naturally
    /// aligned; statics get one 8-byte slot each.
    fn compute(cf: &ClassFile, super_klass: Option<&'static Klass>) -> FieldLayout {
        let mut fields: Vec<FieldDesc> = cf
            .fields
            .iter()
            .map(|f| FieldDesc {
                name: f.name,
                desc: f.desc,
                access: f.access,
                ty: BasicType::from_desc(f.desc.as_str().as_bytes()[0]),
                offset: 0,
                constant_value: f.constant_value,
            })
            .collect();
        for &(name, desc) in injected_fields(cf.name.as_str()) {
            fields.push(FieldDesc {
                name: Symbol::intern(name),
                desc: Symbol::intern(desc),
                access: 0x1000, // ACC_SYNTHETIC: hidden from reflection
                ty: BasicType::from_desc(desc.as_bytes()[0]),
                offset: 0,
                constant_value: 0,
            });
        }
        let mut size = super_klass.map_or(HEADER_SIZE, |s| s.instance_size());
        let mut ref_offsets = super_klass.map_or(Vec::new(), |s| s.ref_offsets.to_vec());
        let mut static_slots = 0;
        for f in fields.iter_mut().filter(|f| f.is_static()) {
            f.offset = static_slots;
            static_slots += 1;
        }
        // The header's free 4 bytes (`heap::HEADER_GAP`) take the first fields of up to 4 bytes
        // that fit, after what superclasses put there.
        let mut gap = super_klass.map_or(HEADER_GAP, |s| {
            s.supers().flat_map(|c| c.fields.iter()).filter(|f| !f.is_static() && f.offset < HEADER_SIZE).map(|f| f.offset + f.ty.size()).max().unwrap_or(HEADER_GAP)
        });
        for width in [8, 4, 2, 1] {
            for f in fields.iter_mut().filter(|f| !f.is_static() && f.ty.size() == width) {
                if width <= 4 && gap.next_multiple_of(width) + width <= 8 {
                    f.offset = gap.next_multiple_of(width);
                    gap = f.offset + width;
                    if f.ty.is_ref() {
                        ref_offsets.push(f.offset);
                    }
                    continue;
                }
                size = (size + width - 1) & !(width - 1);
                f.offset = size;
                if f.ty.is_ref() {
                    ref_offsets.push(size);
                }
                size += width;
            }
        }
        FieldLayout { fields, instance_size: (size + 7) & !7, ref_offsets, static_slots: static_slots as usize }
    }
}

/// JVMS 5.4.5 overriding: `m` overrides `sup` if names/descriptors match and `sup` is accessible
/// from `m`'s class (public/protected, or package-private in the same runtime package).
fn overrides(m: &Method, sup: &Method) -> bool {
    if m.name != sup.name || m.desc != sup.desc || sup.is_private() || sup.is_static() {
        return false;
    }
    const PUBLIC_OR_PROTECTED: u16 = 0x0005;
    sup.access & PUBLIC_OR_PROTECTED != 0
        || (m.holder.package() == sup.holder.package()
            && std::ptr::eq(m.holder.loader(), sup.holder.loader()))
}

fn build_vtable(klass: &'static Klass) -> Box<[&'static Method]> {
    if klass.is_interface() {
        // Interfaces dispatch Object's public methods like any class does.
        return klass.super_klass.map_or(Box::new([]), |s| s.vtable().into());
    }
    let mut vt: Vec<&'static Method> = klass.super_klass.map_or(Vec::new(), |s| s.vtable().to_vec());
    for m in klass.methods().iter().filter(|m| m.is_virtual()) {
        let mut placed = false;
        for (i, slot) in vt.iter_mut().enumerate() {
            if overrides(m, slot) {
                // Before any instance of this class exists, so code that assumed the method
                // unique deoptimizes first.
                slot.mark_overridden();
                *slot = m;
                if !placed {
                    m.set_vtable_index(i as i32);
                    placed = true;
                }
            }
        }
        if !placed {
            m.set_vtable_index(vt.len() as i32);
            vt.push(m);
        }
    }
    // Default methods inherited from interfaces take slots the class does not fill.
    for iface in klass.all_interfaces.iter() {
        for d in iface.methods().iter().filter(|d| d.is_default_candidate()) {
            if !vt.iter().any(|m| m.name == d.name && m.desc == d.desc) {
                vt.push(d);
            }
        }
    }
    vt.into_boxed_slice()
}

/// Convenience for callers holding no `Vm` reference.
pub fn load(name: &str) -> LoadResult {
    vm().load_boot_class(Symbol::intern(name))
}

/// `load` of a literal name, looked up once per call site (natives ask for the same few
/// classes on every call: interning the name and searching the class table each time showed up
/// in method-handle-heavy code).
macro_rules! boot_class {
    ($name:literal) => {{
        static K: std::sync::OnceLock<&'static $crate::klass::Klass> = std::sync::OnceLock::new();
        match K.get() {
            Some(&k) => Ok(k),
            None => $crate::loader::load($name).map(|k| *K.get_or_init(|| k)),
        }
    }};
}
pub(crate) use boot_class;
