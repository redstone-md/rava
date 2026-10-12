//! VM natives: Rust implementations of `native` methods, registered by (class, name,
//! descriptor) exactly as `JNI` would bind them. These are the VM's own natives (the set HotSpot
//! implements in libjvm plus the few libjava ones the boot needs), not library replacements.

mod array;
mod classloader;
mod fs;
pub(crate) mod io;
mod arraycopy;
mod lang;
pub(crate) mod misc;
mod nio_ch;
mod auth;
mod desktop;
mod digest;
mod jfr;
mod management;
pub(crate) mod math;
mod process;
pub(crate) mod mh;
mod module;
mod net;
mod netif;
pub(crate) mod props;
pub(crate) mod reflect;
mod records;
mod reflect_class;
mod stackwalk;
pub(crate) mod thread;
pub(crate) mod unsafe_mem;
mod unixfs;
mod unixfs_ext;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod bsd;
mod posix;
#[cfg(target_os = "linux")]
mod watch;
mod datagram;
mod zip;
mod throwable;

pub(crate) use jfr::{register_on_init as jfr_register_on_init, roots as jfr_roots};
pub(crate) use lang::{class_modifiers, identity_hash, HASH_MASK, HASH_SHIFT};
pub(crate) use module::{roots as module_roots, set_mirror_module, set_primitive_mirror_module};
pub use props::string_array;
pub use throwable::{backtrace_of, describe as describe_throwable};

use crate::method::Method;
use crate::thread::{JResult, Thread};
use std::collections::HashMap;

/// A native body. `args` holds all parameter slots, receiver first for instance methods;
/// long/double values sit in the first of their two slots.
pub type NativeFn = fn(&mut Thread, &[u64]) -> JResult<u64>;

pub struct NativeRegistry {
    table: HashMap<(&'static str, &'static str, &'static str), NativeFn>,
    /// Bodies the VM supplies for JDK methods that have bytecode (HotSpot's intrinsic stubs for
    /// `@IntrinsicCandidate` methods): the method runs as if native.
    stubs: HashMap<(&'static str, &'static str, &'static str), NativeFn>,
}

impl NativeRegistry {
    pub fn with_builtins() -> NativeRegistry {
        let mut r = NativeRegistry { table: HashMap::new(), stubs: HashMap::new() };
        lang::register(&mut r);
        reflect::register(&mut r);
        unsafe_mem::register(&mut r);
        props::register(&mut r);
        thread::register(&mut r);
        io::register(&mut r);
        fs::register(&mut r);
        module::register(&mut r);
        classloader::register(&mut r);
        reflect_class::register(&mut r);
        records::register(&mut r);
        mh::register(&mut r);
        array::register(&mut r);
        unixfs::register(&mut r);
        unixfs_ext::register(&mut r);
        #[cfg(target_os = "linux")]
        watch::register(&mut r);
        #[cfg(target_os = "linux")]
        linux::register(&mut r);
        #[cfg(target_os = "macos")]
        bsd::register(&mut r);
        datagram::register(&mut r);
        zip::register(&mut r);
        nio_ch::register(&mut r);
        process::register(&mut r);
        management::register(&mut r);
        auth::register(&mut r);
        desktop::register(&mut r);
        jfr::register(&mut r);
        stackwalk::register(&mut r);
        net::register(&mut r);
        netif::register(&mut r);
        misc::register(&mut r);
        digest::register(&mut r);
        math::register(&mut r);
        r
    }

    pub(crate) fn add(&mut self, class: &'static str, name: &'static str, desc: &'static str, f: NativeFn) {
        self.table.insert((class, name, desc), f);
    }

    pub(crate) fn add_stub(&mut self, class: &'static str, name: &'static str, desc: &'static str, f: NativeFn) {
        self.stubs.insert((class, name, desc), f);
    }

    pub fn lookup(&self, m: &Method) -> Option<NativeFn> {
        let key = (m.holder.name.as_str(), m.name.as_str(), m.desc.as_str());
        self.table.get(&key).or_else(|| self.stubs.get(&key)).copied()
    }

    /// Whether the VM supplies the body of this method, which has bytecode of its own.
    pub fn has_stub(&self, class: &str, name: &str, desc: &str) -> bool {
        !self.stubs.is_empty() && self.stubs.contains_key(&(class, name, desc))
    }
}

/// A new byte[] with `bytes` (null for `None`).
pub(crate) fn byte_array(t: &mut crate::thread::Thread, bytes: Option<&[u8]>) -> JResult<crate::heap::Oop> {
    let Some(src) = bytes else { return Ok(std::ptr::null_mut()) };
    let byte_class = crate::vm::vm().primitive_array_class(crate::types::BasicType::Byte).expect("[B");
    let arr = crate::runtime::alloc_array(t, byte_class, src.len() as i32)?;
    // SAFETY: a fresh byte[] of the same length.
    unsafe { crate::heap::obj::array_slice::<u8>(arr).copy_from_slice(src) };
    Ok(arr)
}

/// Native return helpers: slot encodings shared with the interpreter.
pub(crate) fn ret_void() -> JResult<u64> {
    Ok(0)
}
pub(crate) fn ret_int(v: i32) -> JResult<u64> {
    Ok(v as i64 as u64)
}
pub(crate) fn ret_bool(v: bool) -> JResult<u64> {
    Ok(v as u64)
}
pub(crate) fn ret_long(v: i64) -> JResult<u64> {
    Ok(v as u64)
}
pub(crate) fn ret_ref(v: crate::heap::Oop) -> JResult<u64> {
    Ok(v as u64)
}
