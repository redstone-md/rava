//! Names for offsets into native modules: on Windows through dbghelp, from the module's PDB
//! when one sits next to it (a `release-diag` build of rava has one) or its exports (system
//! libraries). Each module is loaded once, at a base of its own; names are cached.

use std::collections::HashMap;

#[derive(Default)]
pub struct Symbols {
    /// Module path to the base dbghelp loaded it at (0: it could not).
    loaded: HashMap<String, u64>,
    names: HashMap<(String, u64), Option<String>>,
    next_base: u64,
    off: bool,
}

impl Symbols {
    pub fn new(off: bool) -> Symbols {
        Symbols { off, next_base: 0x1_0000_0000, ..Default::default() }
    }

    pub fn name(&mut self, module: &str, offset: u64) -> Option<String> {
        if self.off {
            return None;
        }
        let key = (module.to_string(), offset);
        if let Some(n) = self.names.get(&key) {
            return n.clone();
        }
        let n = self.resolve(module, offset);
        self.names.insert(key, n.clone());
        n
    }

    #[cfg(windows)]
    fn resolve(&mut self, module: &str, offset: u64) -> Option<String> {
        let base = match self.loaded.get(module) {
            Some(&b) => b,
            None => {
                let b = dbghelp::load(module, self.next_base);
                self.next_base += 0x1_0000_0000;
                self.loaded.insert(module.to_string(), b);
                b
            }
        };
        (base != 0).then(|| dbghelp::name(base + offset)).flatten()
    }

    #[cfg(not(windows))]
    fn resolve(&mut self, _: &str, _: u64) -> Option<String> {
        None
    }
}

#[cfg(windows)]
mod dbghelp {
    use std::sync::Once;

    #[link(name = "dbghelp")]
    extern "system" {
        fn SymInitializeW(process: isize, path: *const u16, invade: i32) -> i32;
        fn SymSetOptions(options: u32) -> u32;
        fn SymLoadModuleExW(process: isize, file: isize, image: *const u16, module: *const u16, base: u64, size: u32, data: *const u8, flags: u32) -> u64;
        fn SymFromAddr(process: isize, address: u64, displacement: *mut u64, info: *mut u8) -> i32;
    }

    /// A pseudo process handle: dbghelp only needs an identifier when nothing is invaded.
    const PROCESS: isize = 1;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn init() {
        static INIT: Once = Once::new();
        // SAFETY: initializes dbghelp once for the pseudo handle, with no search path (each
        // module's own directory is searched for its PDB).
        INIT.call_once(|| unsafe {
            // SYMOPT_UNDNAME: plain function names.
            SymSetOptions(0x2);
            SymInitializeW(PROCESS, std::ptr::null(), 0);
        });
    }

    pub fn load(path: &str, base: u64) -> u64 {
        init();
        if !std::path::Path::new(path).exists() {
            return 0;
        }
        let image = wide(path);
        // SAFETY: a module file loaded for its symbols at a base this process does not use.
        unsafe { SymLoadModuleExW(PROCESS, 0, image.as_ptr(), std::ptr::null(), base, 0, std::ptr::null(), 0) }
    }

    pub fn name(address: u64) -> Option<String> {
        const MAX: usize = 1024;
        // SYMBOL_INFO: SizeOfStruct (88) and MaxNameLen at 80, the name from 84.
        let mut buf = vec![0u8; 88 + MAX];
        buf[0..4].copy_from_slice(&88u32.to_le_bytes());
        buf[80..84].copy_from_slice(&(MAX as u32).to_le_bytes());
        let mut disp = 0u64;
        // SAFETY: a SYMBOL_INFO buffer with room for MAX name bytes.
        if unsafe { SymFromAddr(PROCESS, address, &mut disp, buf.as_mut_ptr()) } == 0 {
            return None;
        }
        // The function only: offsets within it would split its time.
        let len = u32::from_le_bytes(buf[76..80].try_into().unwrap()) as usize;
        Some(String::from_utf8_lossy(&buf[84..84 + len.min(MAX)]).into_owned())
    }
}
