/// mimalloc for the VM's own allocations: compiler threads allocate and free small structures
/// all the time, and glibc's malloc took about half of their time in its lists (All The Mods
/// 10: compilation 178 -> 152 s of CPU, class parsing 9.7 -> 7.3 s).
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

extern "C" {
    fn mi_option_set(option: i32, value: std::ffi::c_long);
}

/// `mi_option_purge_delay` (mimalloc.h): milliseconds before freed memory goes back to the
/// system.
const MI_OPTION_PURGE_DELAY: i32 = 15;

fn main() -> std::process::ExitCode {
    // Freed memory goes back after 100 ms: compilations, back to back, reuse what the last one
    // freed (at once, the kernel cleared those pages again for each: compile CPU on All The
    // Mods 10 152 s; 100 ms 130 s and 7.26 GB resident, against 7.09 at once and 7.37 at 1 s).
    // SAFETY: a plain option setter, safe at any time.
    unsafe { mi_option_set(MI_OPTION_PURGE_DELAY, 100) };
    rava::launcher::main()
}
