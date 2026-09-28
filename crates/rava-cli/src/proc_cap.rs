//! OS-enforced process memory cap (Windows Job Object).
//!
//! `mem_meter` (RAVA_HARD_MB) already aborts cooperatively on every Rust allocation,
//! but that only sees memory routed through Rust's global allocator. This adds a
//! HARD, OS-level ceiling: the process is placed in a Job Object with a committed-
//! memory limit, so ANY allocation past the limit (incl. mmap / Cranelift JIT arena
//! / anything the meter can't see) simply fails — the kernel will not let the
//! process exceed it. This is the true backstop that protects the host from a
//! runaway alloc leak (the prior 31 GB host-freeze). Set via RAVA_PROC_CAP_MB.

/// Apply a hard committed-memory ceiling of `mb` MiB to the current process.
/// Returns true on success. Windows only; a no-op (false) elsewhere.
#[cfg(windows)]
pub fn apply(mb: u64) -> bool {
    use std::os::raw::c_void;
    type Handle = *mut c_void;

    // JobObjectExtendedLimitInformation
    const INFO_CLASS: i32 = 9;
    const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
    const JOB_OBJECT_LIMIT_JOB_MEMORY: u32 = 0x0000_0200;

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_op: u64,
        write_op: u64,
        other_op: u64,
        read_tx: u64,
        write_tx: u64,
        other_tx: u64,
    }
    #[repr(C)]
    struct BasicLimit {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    struct ExtendedLimit {
        basic: BasicLimit,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    extern "system" {
        fn CreateJobObjectW(attrs: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn GetCurrentProcess() -> Handle;
    }

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return false;
        }
        let bytes = (mb as usize).saturating_mul(1024 * 1024);
        // SAFETY: `ExtendedLimit` mirrors JOBOBJECT_EXTENDED_LIMIT_INFORMATION
        // (repr(C), same field order/types) — zeroed then only the limit fields set.
        let mut info: ExtendedLimit = std::mem::zeroed();
        info.basic.limit_flags = JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.process_memory_limit = bytes;
        info.job_memory_limit = bytes;
        let set_ok = SetInformationJobObject(
            job,
            INFO_CLASS,
            &info as *const _ as *const c_void,
            std::mem::size_of::<ExtendedLimit>() as u32,
        );
        if set_ok == 0 {
            return false;
        }
        // Assign SELF to the job. The handle is intentionally NOT closed: the limit
        // must outlive this call for the whole process lifetime.
        AssignProcessToJobObject(job, GetCurrentProcess()) != 0
    }
}

#[cfg(not(windows))]
pub fn apply(_mb: u64) -> bool {
    false
}
