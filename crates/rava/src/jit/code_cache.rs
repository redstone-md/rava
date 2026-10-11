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
        let map = |hint: usize, flags: i32| {
            // SAFETY: anonymous private mapping, committed lazily; MAP_FIXED_NOREPLACE never
            // replaces an existing mapping.
            let p = unsafe {
                libc::mmap(
                    hint as *mut libc::c_void,
                    RESERVE,
                    libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | flags,
                    -1,
                    0,
                )
            };
            (p != libc::MAP_FAILED).then_some(p as usize)
        };
        // Just below the text, else anywhere.
        let hint = (text & !((2 << 20) - 1)).saturating_sub(RESERVE + GAP);
        let p = (hint != 0).then(|| map(hint, libc::MAP_FIXED_NOREPLACE)).flatten().or_else(|| map(0, 0)).expect("cannot reserve the code cache");
        // Transparent huge pages where the system offers them on request: hot code spread over
        // tens of megabytes then takes a few TLB entries instead of thousands. Memory is still
        // committed only as code is written (one huge page at a time).
        // SAFETY: advice on the mapping just made; failure leaves ordinary pages.
        unsafe { libc::madvise(p as *mut libc::c_void, RESERVE, libc::MADV_HUGEPAGE) };
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
    drop(r);
    if !code.sites.is_empty() {
        install_sites(at, &code.sites);
    }
    at
}

/// The call sites (displacement addresses) of each method (by address) that `repoint` keeps
/// pointed at its entry.
fn sites() -> &'static Mutex<std::collections::HashMap<usize, Vec<usize>>> {
    static SITES: std::sync::OnceLock<Mutex<std::collections::HashMap<usize, Vec<usize>>>> = std::sync::OnceLock::new();
    SITES.get_or_init(Default::default)
}

/// Points the `call rel32`s of code just installed at `at` (`Asm::call_method`, not yet
/// running) at their methods' entries, HotSpot's direct calls: no trampoline jump on the way,
/// one code line fewer per call. A displacement within one aligned 8-byte word is repointed
/// later by an atomic store of that word; the others call the method's trampoline for good.
fn install_sites(at: usize, list: &[(u32, usize)]) {
    use std::sync::atomic::Ordering::Acquire;
    let _g = TRAMPOLINES.lock().unwrap();
    let mut map = sites().lock().unwrap();
    for &(pos, m) in list {
        let d = at + pos as usize;
        // SAFETY: methods are immortal.
        let method: &'static crate::method::Method = unsafe { &*(m as *const crate::method::Method) };
        let target = if d % 8 <= 4 {
            map.entry(m).or_default().push(d);
            method.entry.load(Acquire)
        } else {
            trampoline(method)
        };
        let rel = i32::try_from(target as i64 - (d + 4) as i64).expect("call within reach");
        // SAFETY: a displacement of code no thread runs yet.
        unsafe { std::ptr::write_unaligned(d as *mut i32, rel) };
    }
}

/// Stores `rel` into the displacement at `d` (within one aligned 8-byte word) atomically, for
/// threads running the call meanwhile to see the old target or the new one.
fn patch(d: usize, rel: i32) {
    use std::sync::atomic::{AtomicU64, Ordering};
    let word = d & !7;
    let shift = 8 * (d - word) as u32;
    let mask = 0xFFFF_FFFFu64 << shift;
    // SAFETY: an aligned word of installed code, which stays mapped.
    let cell = unsafe { &*(word as *const AtomicU64) };
    let mut old = cell.load(Ordering::Acquire);
    loop {
        let new = (old & !mask) | ((rel as u32 as u64) << shift);
        match cell.compare_exchange_weak(old, new, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(x) => old = x,
        }
    }
}

/// Serializes making and repointing trampolines and direct call sites (`install_sites`,
/// `repoint`).
static TRAMPOLINES: Mutex<()> = Mutex::new(());

/// The address of a `jmp rel32` to `m`'s current entry, made on first use (HotSpot's static
/// call stubs): what a direct call site whose displacement cannot be repointed atomically
/// calls instead (`install_sites`). Under the trampolines' lock.
fn trampoline(m: &'static crate::method::Method) -> usize {
    use std::sync::atomic::Ordering::{Acquire, Release};
    let t = m.trampoline.load(Acquire);
    if t != 0 {
        return t;
    }
    // A 3-byte nop puts the displacement at offset 4: aligned, so a repoint writes it whole.
    let code = super::asm::Code { bytes: vec![0x0F, 0x1F, 0x00, 0xE9, 0, 0, 0, 0], relocs: vec![(4, m.entry.load(Acquire))], sites: Vec::new() };
    let t = install(&code);
    m.trampoline.store(t, Release);
    t
}

/// Points `m`'s trampoline (if any) at its current entry, after `Method::entry` changed.
pub fn repoint(m: &crate::method::Method) {
    use std::sync::atomic::Ordering::Acquire;
    if m.trampoline.load(Acquire) == 0 {
        return;
    }
    let _g = TRAMPOLINES.lock().unwrap();
    let entry = m.entry.load(Acquire);
    let t = m.trampoline.load(Acquire);
    if t != 0 {
        let rel = i32::try_from(entry as i64 - (t + 8) as i64).expect("entry within reach");
        // SAFETY: the aligned displacement of the trampoline's jump; an aligned 4-byte store is
        // atomic, so a thread executing the jump meanwhile goes to the old entry or the new one.
        unsafe { (*((t + 4) as *const std::sync::atomic::AtomicI32)).store(rel, std::sync::atomic::Ordering::Release) };
    }
    if let Some(list) = sites().lock().unwrap().get(&(m as *const crate::method::Method as usize)) {
        for &d in list {
            patch(d, i32::try_from(entry as i64 - (d + 4) as i64).expect("entry within reach"));
        }
    }
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
