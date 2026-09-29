//! Executable memory for compiled methods: one reserved read-write-execute region, bump
//! allocated. Code is never freed (methods of boot and application classes never unload).

use std::sync::Mutex;

const RESERVE: usize = 1 << 30;

struct Region {
    base: usize,
    top: usize,
}

fn region() -> &'static Mutex<Region> {
    static R: std::sync::OnceLock<Mutex<Region>> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        // SAFETY: anonymous private mapping, committed lazily.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                RESERVE,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        assert!(p != libc::MAP_FAILED, "cannot reserve the code cache");
        Mutex::new(Region { base: p as usize, top: p as usize })
    })
}

/// Copies `code` into executable memory and returns its address.
pub fn install(code: &[u8]) -> usize {
    let mut r = region().lock().unwrap();
    let at = (r.top + 15) & !15;
    assert!(at + code.len() <= r.base + RESERVE, "code cache exhausted");
    // SAFETY: `at..at+len` is inside the reserved writable region and not yet handed out.
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len()) };
    r.top = at + code.len();
    at
}

/// Appends `address size name` to `/tmp/perf-<pid>.map` when `RAVA_CORE_PERF_MAP` is set, so
/// `perf` can name compiled methods.
pub fn perf_map(address: usize, size: usize, name: &str) {
    use std::io::Write;
    static FILE: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let file = FILE.get_or_init(|| {
        std::env::var_os("RAVA_CORE_PERF_MAP")?;
        let path = format!("/tmp/perf-{}.map", std::process::id());
        std::fs::File::create(path).ok().map(Mutex::new)
    });
    if let Some(f) = file {
        let _ = writeln!(f.lock().unwrap(), "{address:x} {size:x} {name}");
    }
}
