//! Executable memory for compiled methods: one reserved read-write-execute region, bump
//! allocated. Code is never freed (methods of boot and application classes never unload).
//! The region is placed within reach of rava's own text where possible, so compiled code calls
//! runtime helpers with 5-byte relative calls (`Asm::call_abs`).

use std::sync::Mutex;

const RESERVE: usize = 1 << 30;

struct Region {
    base: usize,
    top: usize,
    /// Every address of the region reaches rava's text with a 32-bit displacement.
    near: bool,
}

/// How far below rava's text the region ends (room for the text itself to stay in reach).
const GAP: usize = 64 << 20;

/// Whether a 32-bit displacement from anywhere in `base..base + RESERVE` reaches `text`.
fn reaches(base: usize, text: usize) -> bool {
    let far = |a: usize| (a as i64 - text as i64).unsigned_abs() as usize;
    far(base).max(far(base + RESERVE)) < (1usize << 31) - GAP
}

fn region() -> &'static Mutex<Region> {
    static R: std::sync::OnceLock<Mutex<Region>> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        let text = install as *const () as usize;
        // Just below the text, else anywhere.
        let hint = (text & !((2 << 20) - 1)).saturating_sub(RESERVE + GAP);
        let p = (hint != 0)
            .then(|| crate::os::reserve_growing(RESERVE, Some(hint), true))
            .flatten()
            .or_else(|| crate::os::reserve_growing(RESERVE, None, true))
            .expect("cannot reserve the code cache");
        // Transparent huge pages where the system offers them on request: hot code spread over
        // tens of megabytes then takes a few TLB entries instead of thousands. Memory is still
        // committed only as code is written (one huge page at a time).
        crate::os::huge_pages(p, RESERVE);
        BASE.store(p, std::sync::atomic::Ordering::Release);
        Mutex::new(Region { base: p, top: p, near: reaches(p, text) })
    })
}

/// The region's start, for `contains` (0 before it exists).
static BASE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Whether `pc` is in the code cache (lock-free: fault handlers ask).
pub fn contains(pc: usize) -> bool {
    let base = BASE.load(std::sync::atomic::Ordering::Acquire);
    base != 0 && (base..base + RESERVE).contains(&pc)
}

/// Bytes of compiled code so far (`memlog`).
pub fn used_bytes() -> usize {
    let r = region().lock().unwrap();
    r.top - r.base
}

/// Whether code may call rava's functions with 32-bit displacements.
pub fn near() -> bool {
    region().lock().unwrap().near
}

/// Copies `code` into executable memory, resolving its calls to absolute addresses, and
/// returns its address.
pub fn install(code: &super::asm::Code) -> usize {
    let mut r = region().lock().unwrap();
    let at = (r.top + 15) & !15;
    assert!(at + code.len() <= r.base + RESERVE, "code cache exhausted");
    // SAFETY: `at..at+len` is inside the reserved writable region and not yet handed out.
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len()) };
    for &(pos, target) in &code.relocs {
        let rel = i32::try_from(target as i64 - (at + pos as usize + 4) as i64).expect("call within reach");
        // SAFETY: a displacement inside the code just copied.
        unsafe { std::ptr::write_unaligned((at + pos as usize) as *mut i32, rel) };
    }
    r.top = at + code.len();
    at
}

/// Serializes making and repointing trampolines (`direct_entry`, `repoint`).
static TRAMPOLINES: Mutex<()> = Mutex::new(());

/// The address of a `jmp rel32` to `m`'s current entry, for compiled code to call `m` with a
/// direct `call rel32` (HotSpot's static call stubs): the target is known as the call decodes,
/// where an indirect call through `Method::entry` waits for its load whenever the branch
/// predictor has lost it. Made on first use; None when the code cache is out of reach of rava's
/// text (the adapters an entry may be).
pub fn direct_entry(m: &'static crate::method::Method) -> Option<usize> {
    use std::sync::atomic::Ordering::{Acquire, Release};
    if !near() {
        return None;
    }
    let _g = TRAMPOLINES.lock().unwrap();
    let t = m.trampoline.load(Acquire);
    if t != 0 {
        return Some(t);
    }
    // A 3-byte nop puts the displacement at offset 4: aligned, so a repoint writes it whole.
    let code = super::asm::Code { bytes: vec![0x0F, 0x1F, 0x00, 0xE9, 0, 0, 0, 0], relocs: vec![(4, m.entry.load(Acquire))] };
    let t = install(&code);
    m.trampoline.store(t, Release);
    Some(t)
}

/// Points `m`'s trampolines (if any) at its current entries, after `Method::entry` changed.
pub fn repoint(m: &crate::method::Method) {
    use std::sync::atomic::Ordering::Acquire;
    // Always under the lock, even with no trampoline yet: one being made meanwhile read the
    // entry before it changed, and is published before this runs and aims it again.
    let _g = TRAMPOLINES.lock().unwrap();
    let t = m.trampoline.load(Acquire);
    if t != 0 {
        aim(t, m.entry.load(Acquire));
    }
    let t = m.reg_trampoline.load(Acquire);
    if t != 0 {
        let target = reg_target(m);
        aim(t, target);
        m.reg_call.store(target, std::sync::atomic::Ordering::Release);
    }
}

/// Points trampoline `t` at `target`.
fn aim(t: usize, target: usize) {
    let rel = i32::try_from(target as i64 - (t + 8) as i64).expect("entry within reach");
    // SAFETY: the aligned displacement of the trampoline's jump; an aligned 4-byte store is
    // atomic, so a thread executing the jump meanwhile goes to the old entry or the new one.
    unsafe { (*((t + 4) as *const std::sync::atomic::AtomicI32)).store(rel, std::sync::atomic::Ordering::Release) };
}

/// A trampoline for direct calls to `m` in the register convention (`jit::abi`), repointed
/// with `m`'s entry; None when code cannot reach it with a 32-bit displacement.
pub fn reg_entry(m: &'static crate::method::Method) -> Option<usize> {
    use std::sync::atomic::Ordering::{Acquire, Release};
    if !near() {
        return None;
    }
    let _g = TRAMPOLINES.lock().unwrap();
    let t = m.reg_trampoline.load(Acquire);
    if t != 0 {
        return Some(t);
    }
    let target = reg_target(m);
    let code = super::asm::Code { bytes: vec![0x0F, 0x1F, 0x00, 0xE9, 0, 0, 0, 0], relocs: vec![(4, target)] };
    let t = install(&code);
    m.reg_trampoline.store(t, Release);
    m.reg_call.store(target, Release);
    Some(t)
}

/// Where a register-convention call to `m` goes now: the register entry of the code `m.entry`
/// runs, when that code has one, else `m`'s adapter (caller holds `TRAMPOLINES`).
fn reg_target(m: &crate::method::Method) -> usize {
    use std::sync::atomic::Ordering::Acquire;
    let entry = m.entry.load(Acquire);
    match m.compiled() {
        Some(cm) if cm.entry == entry && cm.reg_entry != 0 => cm.reg_entry,
        _ => reg_adapter(m),
    }
}

/// `m`'s adapter from the register convention to its frame-slot entry: stores each register
/// argument where the interpreter keeps it (rsi is the callee's frame), then enters through
/// `m.entry` with the thread in rdi as that entry expects.
fn reg_adapter(m: &crate::method::Method) -> usize {
    use super::abi::ArgReg;
    use super::asm::{mem, X, R};
    use std::sync::atomic::Ordering::{Acquire, Release};
    let made = m.reg_adapter.load(Acquire);
    if made != 0 {
        return made;
    }
    let mut a = super::asm::Asm::default();
    for (slot, reg, _) in super::abi::arg_regs(m) {
        match reg {
            ArgReg::Gpr(r) => a.store(true, mem(R::Rsi, 8 * slot as i32), r),
            ArgReg::Xmm(x) => a.fstore(true, mem(R::Rsi, 8 * slot as i32), X(x)),
        }
    }
    a.mov_rr(true, R::Rdi, R::R12);
    a.mov_ri(R::Rdx, m as *const crate::method::Method as i64);
    a.load(true, R::Rax, mem(R::Rdx, std::mem::offset_of!(crate::method::Method, entry) as i32));
    a.jmp_r(R::Rax);
    let at = install(&a.finish());
    m.reg_adapter.store(at, Release);
    at
}

/// Appends `address size name` to `/tmp/perf-<pid>.map` when `RAVA_PERF_MAP` is set, so
/// `perf` can name compiled methods.
pub fn perf_map(address: usize, size: usize, name: &str) {
    use std::io::Write;
    static FILE: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    if KEEP_NAMES.load(std::sync::atomic::Ordering::Relaxed) {
        NAMES.lock().unwrap().insert(address, (size, name.to_string()));
    }
    let Some(mode) = env_once!("RAVA_PERF_MAP") else { return };
    // `exit`: kept in memory and written when the VM exits (`flush_perf_map`), no write per
    // compilation (tools that serialize threads, such as valgrind, stall on them).
    if mode == "exit" {
        PERF_LINES.lock().unwrap().push(format!("{address:x} {size:x} {name}"));
        return;
    }
    let file = FILE.get_or_init(|| {
        let path = format!("/tmp/perf-{}.map", std::process::id());
        std::fs::File::create(path).ok().map(Mutex::new)
    });
    if let Some(f) = file {
        let _ = writeln!(f.lock().unwrap(), "{address:x} {size:x} {name}");
    }
}

static PERF_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Compiled code's names by address, kept for the sampling profiler (`sampler.rs`).
static KEEP_NAMES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NAMES: Mutex<std::collections::BTreeMap<usize, (usize, String)>> = Mutex::new(std::collections::BTreeMap::new());

/// Keeps the name of the code compiled from now on (`names`).
pub fn keep_names() {
    KEEP_NAMES.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The kept names: start address to (size, name).
pub fn names() -> std::collections::BTreeMap<usize, (usize, String)> {
    NAMES.lock().unwrap().clone()
}

/// Writes the perf map kept in memory (`RAVA_PERF_MAP=exit`).
pub fn flush_perf_map() {
    let lines = std::mem::take(&mut *PERF_LINES.lock().unwrap());
    if !lines.is_empty() {
        let _ = std::fs::write(format!("/tmp/perf-{}.map", std::process::id()), lines.join("\n") + "\n");
    }
}

/// Makes `m`'s register trampoline and returns where a register-convention call to `m` goes
/// (`Method::reg_call`): the slow path of a vtable call that finds it not made yet.
extern "sysv64" fn make_reg_call(m: &'static crate::method::Method) -> usize {
    reg_entry(m);
    m.reg_call.load(std::sync::atomic::Ordering::Acquire)
}

/// A stub that makes the register trampoline of the method in rdx and returns its
/// `Method::reg_call` in rax, keeping every argument register (`jit::abi`) and rdx: called by
/// a vtable call whose target has none yet, with the arguments already in place.
pub fn reg_call_stub() -> usize {
    use super::abi::{ARG_GPRS, ARG_XMMS};
    use super::asm::{mem, Alu, Asm, R, X};
    static STUB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *STUB.get_or_init(|| {
        let mut a = Asm::default();
        // Entered with rsp 8 below a 16-byte boundary: five pushes and 32 bytes realign it.
        a.push(R::Rdx);
        for r in ARG_GPRS {
            a.push(r);
        }
        a.alu_ri(true, Alu::Sub, R::Rsp, 8 * ARG_XMMS.len() as i32);
        for (i, &x) in ARG_XMMS.iter().enumerate() {
            a.fstore(true, mem(R::Rsp, 8 * i as i32), X(x));
        }
        a.mov_rr(true, R::Rdi, R::Rdx);
        a.call_abs(make_reg_call as *const () as usize);
        for (i, &x) in ARG_XMMS.iter().enumerate() {
            a.fload(true, X(x), mem(R::Rsp, 8 * i as i32));
        }
        a.alu_ri(true, Alu::Add, R::Rsp, 8 * ARG_XMMS.len() as i32);
        for r in ARG_GPRS.iter().rev() {
            a.pop(*r);
        }
        a.pop(R::Rdx);
        a.ret();
        install(&a.finish())
    })
}
