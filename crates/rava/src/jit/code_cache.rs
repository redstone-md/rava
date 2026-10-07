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
    at
}

/// Appends `address size name` to `/tmp/perf-<pid>.map` when `RAVA_PERF_MAP` is set, so
/// `perf` can name compiled methods.
pub fn perf_map(address: usize, size: usize, name: &str) {
    use std::io::Write;
    static FILE: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let file = FILE.get_or_init(|| {
        std::env::var_os("RAVA_PERF_MAP")?;
        let path = format!("/tmp/perf-{}.map", std::process::id());
        std::fs::File::create(path).ok().map(Mutex::new)
    });
    if let Some(f) = file {
        let _ = writeln!(f.lock().unwrap(), "{address:x} {size:x} {name}");
    }
}
