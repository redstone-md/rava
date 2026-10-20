//! Temporary priority raises for threads a stopped world waits on.
//!
//! While the world is stopped every Java thread waits on two kinds of thread: the collector
//! itself, which may be an application thread at a low Java priority, and threads still running
//! Java on their way to a safepoint. On Windows a below-normal thread gets no processor while
//! every core runs normal-priority work (other processes included) until the balance-set
//! manager boosts it, seconds later, so one such thread held the whole process for seconds per
//! collection. HotSpot collects on its VM thread, at the highest priority; rava raises the
//! threads it waits on for the length of the stop instead.

/// A thread's priority before `raise`, for `restore`.
#[derive(Clone, Copy)]
pub struct Raised {
    handle: usize,
    old: i32,
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_HIGHEST};

    /// What GetThreadPriority returns on failure.
    const ERROR_RETURN: i32 = i32::MAX;

    fn handle(h: usize) -> HANDLE {
        // SAFETY: the calling thread's pseudo-handle.
        if h == 0 { unsafe { GetCurrentThread() } } else { h as HANDLE }
    }

    pub fn raise(h: usize) -> Option<i32> {
        // SAFETY: a thread handle of this process (records keep theirs open), else the calling
        // thread's pseudo-handle.
        let old = unsafe { GetThreadPriority(handle(h)) };
        if old == ERROR_RETURN || old >= THREAD_PRIORITY_HIGHEST {
            return None;
        }
        // SAFETY: as above.
        unsafe { SetThreadPriority(handle(h), THREAD_PRIORITY_HIGHEST) };
        Some(old)
    }

    pub fn set(h: usize, p: i32) {
        // SAFETY: as in `raise`.
        unsafe { SetThreadPriority(handle(h), p) };
    }
}

#[cfg(unix)]
mod imp {
    // Raising a priority needs privileges on Unix, and Java priorities leave scheduling alone
    // there, so no thread runs below the others.
    pub fn raise(_h: usize) -> Option<i32> {
        None
    }

    pub fn set(_h: usize, _p: i32) {}
}

/// Raises thread `handle` (0: the calling thread) to the highest normal priority; None when it
/// already runs at least that high.
pub fn raise(handle: usize) -> Option<Raised> {
    imp::raise(handle).map(|old| Raised { handle, old })
}

/// Gives the thread back the priority it had before `raise`.
pub fn restore(r: Raised) {
    imp::set(r.handle, r.old);
}
