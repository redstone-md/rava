//! Copy-and-patch execution tier + AOT cache (see docs/copy-and-patch-roadmap.md).
//!
//! Lowers a method's bytecode to a straight-line native sequence with NO per-opcode
//! dispatch loop, at a fraction of Cranelift's compile cost. Each opcode is emitted
//! as INLINE x86-64 (true copy-and-patch — no per-op call/ret): the operand stack
//! lives in registers (rsi = stack base, rbx = sp index, rdi = locals base) and each
//! op is a few instructions operating on `[rsi + rbx*8 + disp]`. Branches are real
//! machine-code Jcc over a pc→offset map with rel32 fixups. The emitted code uses only
//! immediates (imm32) and relative jumps (rel32) — it is fully position-independent, so
//! it is trivially AOT-cacheable (serialise the bytes, no relocations to patch).
//!
//! **AOT (`CompiledObj`):** serialise → persist → reload in a later process → `link`
//! (place in an executable page) → run, with zero recompilation. For the C2-quality
//! tier the same cache carries Abs8 relocations to the rava_* callbacks (see the C2 AOT
//! tests); cnp's own inline code needs none.
//!
//! Correctness is gated by differential tests against a reference evaluator. An unsupported
//! shape makes `compile` return `None`, and the caller keeps the ordinary tier. The emitter
//! supports the platform C ABI on Windows and Linux x86-64. Runtime callback addresses are
//! process-local; only relocation-free bodies may use the persisted CNP cache.

#![cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))]

use std::collections::HashMap;
use std::os::raw::c_void;

/// In-memory execution frame. `#[repr(C)]`: the emitter reads these field offsets
/// (stack @0, sp @8, locals @16) directly.
#[repr(C)]
pub struct CnpFrame {
    pub stack: *mut i64,
    pub sp: usize,
    pub locals: *mut i64,
}

#[derive(Clone, Copy)]
enum Cond {
    Eq,
    Ne,
    Lt,
    Ge,
    Gt,
    Le,
}
impl Cond {
    /// Two-byte 0F Jcc opcode for the SIGNED condition (used directly after `cmp`).
    fn jcc(self) -> u8 {
        match self {
            Cond::Eq => 0x84, // je
            Cond::Ne => 0x85, // jne
            Cond::Lt => 0x8C, // jl
            Cond::Ge => 0x8D, // jge
            Cond::Gt => 0x8F, // jg
            Cond::Le => 0x8E, // jle
        }
    }
}

struct Fixup {
    at: usize,
    end: usize,
    target_pc: usize,
}

pub struct CompiledFn {
    code: *mut u8,
    #[allow(dead_code)]
    len: usize,
}
unsafe impl Send for CompiledFn {}
unsafe impl Sync for CompiledFn {}
impl CompiledFn {
    /// # Safety
    /// `frame.stack`/`frame.locals` must be large enough for max_stack/max_locals.
    pub unsafe fn run(&self, frame: *mut CnpFrame) -> i64 {
        let f: extern "C" fn(*mut CnpFrame) -> i64 = std::mem::transmute(self.code);
        f(frame)
    }
    /// Numeric AOT ABI (aot_clif): `extern "C" fn(locals: *mut i64) -> i64`. `locals`
    /// must be ≥ max_locals i64 slots.
    /// # Safety
    /// `locals` must point at a buffer of at least the method's max_locals i64 slots.
    pub unsafe fn run_numeric(&self, locals: *mut i64) -> i64 {
        let f: extern "C" fn(*mut i64) -> i64 = std::mem::transmute(self.code);
        f(locals)
    }
    /// Constant-table AOT ABI (aot_clif): `extern "C" fn(locals: *mut i64, consts:
    /// *const i64) -> i64`. `consts` is the per-method runtime-constant array the linker
    /// filled (field ids, class_ptrs, interned-string addresses).
    /// # Safety
    /// Both pointers must be valid for the method's local / constant counts.
    pub unsafe fn run_const(&self, locals: *mut i64, consts: *const i64) -> i64 {
        let f: extern "C" fn(*mut i64, *const i64) -> i64 = std::mem::transmute(self.code);
        f(locals, consts)
    }
    /// Raw code pointer — lets a caller capture it and drop the borrow before running
    /// (the AOT run-path does this so its heap callbacks can re-enter the VM). The
    /// run_const ABI: `extern "C" fn(*mut i64 locals, *const i64 consts) -> i64`.
    pub fn code_ptr(&self) -> *mut u8 {
        self.code
    }
    /// Exact linked code extent, excluding the executable allocator's page padding.
    pub(crate) fn code_len(&self) -> usize {
        self.len
    }
}
impl Drop for CompiledFn {
    fn drop(&mut self) {
        unsafe { dealloc_exec(self.code, self.len) };
    }
}

#[cfg(windows)]
const MEM_COMMIT: u32 = 0x1000;
#[cfg(windows)]
const MEM_RESERVE: u32 = 0x2000;
#[cfg(windows)]
const MEM_RELEASE: u32 = 0x8000;
#[cfg(windows)]
const PAGE_EXECUTE_READWRITE: u32 = 0x40;
#[cfg(windows)]
extern "system" {
    fn VirtualAlloc(addr: *mut c_void, size: usize, typ: u32, protect: u32) -> *mut c_void;
    fn VirtualFree(addr: *mut c_void, size: usize, typ: u32) -> i32;
}

#[cfg(windows)]
fn alloc_exec(code: &[u8]) -> Option<*mut u8> {
    let p = unsafe {
        VirtualAlloc(
            std::ptr::null_mut(),
            code.len().max(1),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_EXECUTE_READWRITE,
        )
    };
    if p.is_null() {
        return None;
    }
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len()) };
    Some(p as *mut u8)
}

#[cfg(windows)]
unsafe fn dealloc_exec(code: *mut u8, _len: usize) {
    VirtualFree(code as *mut c_void, 0, MEM_RELEASE);
}

#[cfg(target_os = "linux")]
fn alloc_exec(code: &[u8]) -> Option<*mut u8> {
    const PROT_READ: i32 = 0x1;
    const PROT_WRITE: i32 = 0x2;
    const PROT_EXEC: i32 = 0x4;
    const MAP_PRIVATE: i32 = 0x02;
    const MAP_ANONYMOUS: i32 = 0x20;
    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
    }
    let len = code.len().max(1);
    let p = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE | PROT_EXEC,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if p as isize == -1 {
        return None;
    }
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len()) };
    Some(p as *mut u8)
}

#[cfg(target_os = "linux")]
unsafe fn dealloc_exec(code: *mut u8, len: usize) {
    unsafe extern "C" {
        fn munmap(addr: *mut c_void, len: usize) -> i32;
    }
    munmap(code as *mut c_void, len.max(1));
}

fn s16(bc: &[u8], p: usize) -> Option<i32> {
    Some(i16::from_be_bytes([*bc.get(p)?, *bc.get(p + 1)?]) as i32)
}

/// Compile an integer method body to a position-independent `CompiledObj`, or `None`
/// Compile context: getfield resolution (cp index → (interned field id, is-ref)) and the
/// rava_getfield callback address. Empty for pure-int methods (getfield then bails).
#[derive(Default)]
pub struct CnpCtx {
    pub fields: HashMap<u16, (i64, bool)>,
    pub getfield_addr: usize,
}

/// for an unsupported shape (→ caller interprets). Every path must end in a return.
pub fn compile(bc: &[u8]) -> Option<CompiledObj> {
    compile_ctx(bc, &CnpCtx::default())
}

pub fn compile_link(bc: &[u8]) -> Option<CompiledFn> {
    compile(bc)?.link()
}

// ═══════════════ On-disk AOT cache (RAVA_AOT_DIR) ═══════════════
//
// Keyed by the FNV-1a hash of the method's POST-TRANSFORM bytecode: a modloader
// (Fabric mixin, Forge coremod) that rewrites a method changes the bytes → the key
// misses → the method recompiles (and re-caches). Execution is untouched — the cache
// only moves WHERE the compile happened; every boot still runs all code honestly.
// Format-versioned so an emitter change invalidates stale entries wholesale.

/// Cache format version — bump on ANY emitter/ABI change (stale machine code from an
/// older emitter must never link).
pub const AOT_FORMAT: u32 = 4;

/// A per-method runtime constant, stored SYMBOLICALLY in the AOT file (the concrete
/// i64 — a field id, a class pointer, an interned-string address — is per-PROCESS, so
/// it is resolved at LINK time, not baked). The linker fills the method's constant
/// table by resolving each descriptor in the loading process.
#[derive(Clone, Debug, PartialEq)]
pub enum ConstDesc {
    /// Interned field-identity id for `owner#name`.
    FieldId(String, String),
    /// The loaded+initialised class's `Arc::as_ptr` (the JIT's baked class_ptr).
    ClassPtr(String),
    /// A reserved primitive getstatic value-cell slot for `owner#name`.
    StaticSlot(String, String),
    /// A permanent interned ArenaString address for the literal text.
    StrLit(String),
    /// A plain integer immediate (a cp index, an arity) — process-independent, but
    /// carried through the table uniformly so the emitter never bakes an iconst that a
    /// later tier might need to relocate.
    Imm(i64),
    /// A direct-leaf call target, stored as the symbolic owner/name/descriptor. Resolved
    /// at LINK time to the linked leaf function's entry address — or 0 when the direct
    /// arm is disabled/unavailable (the caller then keeps its rava_invoke fallback).
    /// The pointer is NEVER baked into prepared bytes.
    AotLeaf(String, String, String),
}

/// FNV-1a over the bytecode (+ the format version), rendered as the cache file name.
/// A tag that changes whenever the rava binary is rebuilt (the executable's mtime),
/// computed once. The cache stores MACHINE CODE from a specific translator build; a
/// codegen change keeps AOT_FORMAT (the serialization version) but must still invalidate
/// stale code. Keying on the binary mtime makes any rebuild miss the old entries.
fn binary_tag() -> u64 {
    static T: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        std::env::current_exe()
            .and_then(std::fs::metadata)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

pub fn aot_key(bc: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bc {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("cnp{AOT_FORMAT}_{:08x}_{h:016x}_{}", binary_tag(), bc.len())
}

/// The AOT cache directory (RAVA_AOT_DIR), created on first use. None = cache off.
pub(crate) fn aot_dir() -> Option<&'static std::path::PathBuf> {
    static D: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        let p = std::path::PathBuf::from(std::env::var_os("RAVA_AOT_DIR")?);
        std::fs::create_dir_all(&p).ok()?;
        Some(p)
    })
    .as_ref()
}

/// Load-or-compile through the AOT cache: a hit deserialises + links the cached
/// machine code (no compile); a miss compiles and persists the object for the next
/// process. With RAVA_AOT_DIR unset this is exactly `compile_link`.
pub fn compile_link_aot(bc: &[u8]) -> Option<CompiledFn> {
    let Some(dir) = aot_dir() else { return compile_link(bc) };
    let path = dir.join(aot_key(bc));
    if let Ok(bytes) = std::fs::read(&path) {
        if let Some(obj) = CompiledObj::from_bytes(&bytes) {
            if let Some(f) = obj.link() {
                return Some(f);
            }
        }
        // Corrupt/undecodable entry: fall through to a fresh compile + overwrite.
    }
    let obj = compile(bc)?;
    let _ = std::fs::write(&path, obj.to_bytes());
    obj.link()
}
mod asm;
pub(crate) use asm::*;
mod tests;
pub(crate) use tests::*;
