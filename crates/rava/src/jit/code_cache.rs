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
            .then(|| crate::os::reserve(RESERVE, Some(hint), true))
            .flatten()
            .or_else(|| crate::os::reserve(RESERVE, None, true))
            .expect("cannot reserve the code cache");
        // Transparent huge pages where the system offers them on request: hot code spread over
        // tens of megabytes then takes a few TLB entries instead of thousands. Memory is still
        // committed only as code is written (one huge page at a time).
        crate::os::huge_pages(p, RESERVE);
        Mutex::new(Region { base: p, top: p, near: reaches(p, text) })
    })
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

/// Points `m`'s trampoline (if any) at its current entry, after `Method::entry` changed.
pub fn repoint(m: &crate::method::Method) {
    use std::sync::atomic::Ordering::Acquire;
    if m.trampoline.load(Acquire) == 0 {
        return;
    }
    let _g = TRAMPOLINES.lock().unwrap();
    let t = m.trampoline.load(Acquire);
    let rel = i32::try_from(m.entry.load(Acquire) as i64 - (t + 8) as i64).expect("entry within reach");
    // SAFETY: the aligned displacement of the trampoline's jump; an aligned 4-byte store is
    // atomic, so a thread executing the jump meanwhile goes to the old entry or the new one.
    unsafe { (*((t + 4) as *const std::sync::atomic::AtomicI32)).store(rel, std::sync::atomic::Ordering::Release) };
}

/// Appends `address size name` to `/tmp/perf-<pid>.map` when `RAVA_PERF_MAP` is set, so
/// `perf` can name compiled methods.
pub fn perf_map(address: usize, size: usize, name: &str) {
    use std::io::Write;
    static FILE: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
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

/// Writes the perf map kept in memory (`RAVA_PERF_MAP=exit`).
pub fn flush_perf_map() {
    let lines = std::mem::take(&mut *PERF_LINES.lock().unwrap());
    if !lines.is_empty() {
        let _ = std::fs::write(format!("/tmp/perf-{}.map", std::process::id()), lines.join("\n") + "\n");
    }
}
