/// mimalloc for the VM's own allocations: compiler threads allocate and free small structures
/// all the time, and glibc's malloc took about half of their time in its lists (All The Mods
/// 10: compilation 178 -> 152 s of CPU, class parsing 9.7 -> 7.3 s).
#[global_allocator]
static ALLOCATOR: rava::memlog::Counting<mimalloc::MiMalloc> = rava::memlog::Counting(mimalloc::MiMalloc);

extern "C" {
    fn mi_option_set(option: i32, value: std::ffi::c_long);
    fn mi_stats_get_json(buf_size: usize, buf: *mut std::ffi::c_char) -> *mut std::ffi::c_char;
    fn mi_free(p: *mut std::ffi::c_void);
}

/// The bytes mimalloc holds committed now, from its statistics (`"committed": {..., "current": n}`).
fn allocator_committed() -> usize {
    // SAFETY: mimalloc allocates the JSON text, freed with mi_free once read.
    unsafe {
        let p = mi_stats_get_json(0, std::ptr::null_mut());
        if p.is_null() {
            return 0;
        }
        let text = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        mi_free(p.cast());
        text.split_once("\"committed\"")
            .and_then(|(_, rest)| rest.split_once("\"current\":"))
            .and_then(|(_, rest)| rest.trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
            .unwrap_or(0)
    }
}

/// `mi_option_purge_delay` (mimalloc.h): milliseconds before freed memory goes back to the
/// system.
const MI_OPTION_PURGE_DELAY: i32 = 15;

/// `mi_option_page_commit_on_demand`: commit a page's memory as it is used, not whole when the
/// page is set up. Without overcommit (Windows) a committed byte is charged whether or not it is
/// touched.
#[cfg(windows)]
const MI_OPTION_PAGE_COMMIT_ON_DEMAND: i32 = 40;

fn main() -> std::process::ExitCode {
    // Freed memory goes back after 100 ms: compilations, back to back, reuse what the last one
    // freed (at once, the kernel cleared those pages again for each: compile CPU on All The
    // Mods 10 152 s; 100 ms 130 s and 7.26 GB resident, against 7.09 at once and 7.37 at 1 s).
    // SAFETY: a plain option setter, safe at any time.
    // (MIMALLOC_PURGE_DELAY, read by mimalloc itself, still decides when set.)
    if std::env::var_os("MIMALLOC_PURGE_DELAY").is_none() {
        unsafe { mi_option_set(MI_OPTION_PURGE_DELAY, 100) };
    }
    // SAFETY: as above.
    #[cfg(windows)]
    unsafe { mi_option_set(MI_OPTION_PAGE_COMMIT_ON_DEMAND, 1) };
    let _ = rava::memlog::ALLOCATOR_COMMITTED.set(allocator_committed);
    rava::memlog::count_from_start();
    rava::launcher::main()
}
