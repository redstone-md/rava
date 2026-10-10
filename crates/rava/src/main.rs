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
    // Freed memory goes back at once (~90 MB less resident on a modded server, no slower).
    // SAFETY: a plain option setter, safe at any time.
    unsafe { mi_option_set(MI_OPTION_PURGE_DELAY, 0) };
    rava::launcher::main()
}
