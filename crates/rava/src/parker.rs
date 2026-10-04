//! A thread's park event (`LockSupport.park`/`unpark`, `Thread.sleep`, interrupts), as
//! HotSpot's `Parker`, with two changes for cheap hand-offs between threads:
//!
//! - the permit is an atomic flag, and `unpark` takes the lock and wakes only when the owner is
//!   actually blocked (a futex wake is a system call, and an inter-processor interrupt under
//!   virtualization);
//! - `park` first spins briefly for the permit. Producer/consumer hand-offs (an executor's
//!   queue, a future's completion) are usually answered within microseconds, much sooner than a
//!   sleep and wake-up round trip. The spin adapts like HotSpot's monitor spinning: it doubles
//!   after a park the permit reached while spinning and halves after one that had to block, so
//!   threads that wait long burn almost nothing.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::{Relaxed, SeqCst}};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

/// The spin budget in nanoseconds: at least 1 µs, at most 20 µs.
const MIN_SPIN: u32 = 1_000;
const MAX_SPIN: u32 = 20_000;

pub struct Parker {
    permit: AtomicBool,
    /// Threads blocked on `cv` (the owner parking or sleeping).
    sleepers: AtomicU32,
    lock: Mutex<()>,
    cv: Condvar,
    spin: AtomicU32,
}

impl Default for Parker {
    fn default() -> Self {
        Parker { permit: AtomicBool::new(false), sleepers: AtomicU32::new(0), lock: Mutex::new(()), cv: Condvar::new(), spin: AtomicU32::new(MIN_SPIN * 4) }
    }
}

impl Parker {
    /// Makes the permit available and wakes the owner if it is blocked.
    pub fn unpark(&self) {
        self.permit.store(true, SeqCst);
        // Pairs with `block`: either it sees the permit, or this sees it registered.
        if self.sleepers.load(SeqCst) != 0 {
            drop(self.lock.lock().unwrap_or_else(|e| e.into_inner()));
            self.cv.notify_all();
        }
    }

    /// Waits for the permit (consuming it), until `deadline` or while `interrupted` is false.
    pub fn park(&self, deadline: Option<Instant>, interrupted: &dyn Fn() -> bool) {
        if self.permit.swap(false, SeqCst) {
            return;
        }
        let budget = self.spin.load(Relaxed);
        let until = Instant::now() + std::time::Duration::from_nanos(budget as u64);
        loop {
            for _ in 0..32 {
                std::hint::spin_loop();
                if self.permit.load(Relaxed) && self.permit.swap(false, SeqCst) {
                    self.spin.store((budget * 2).min(MAX_SPIN), Relaxed);
                    return;
                }
            }
            if Instant::now() >= until {
                break;
            }
        }
        self.spin.store((budget / 2).max(MIN_SPIN), Relaxed);
        self.block(deadline, &|| self.permit.load(SeqCst) || interrupted());
        self.permit.store(false, SeqCst);
    }

    /// Sleeps until `deadline` unless `woken` (checked after every wake-up) turns true first;
    /// leaves the permit as it is. Returns whether `woken` ended it.
    pub fn sleep(&self, deadline: Instant, woken: &dyn Fn() -> bool) -> bool {
        self.block(Some(deadline), woken)
    }

    fn block(&self, deadline: Option<Instant>, done: &dyn Fn() -> bool) -> bool {
        let mut g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.sleepers.fetch_add(1, SeqCst);
        let woken = loop {
            if done() {
                break true;
            }
            match deadline {
                None => g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner()),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        break false;
                    }
                    g = self.cv.wait_timeout(g, d - now).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        };
        self.sleepers.fetch_sub(1, SeqCst);
        woken
    }
}
