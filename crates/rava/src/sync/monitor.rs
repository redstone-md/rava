//! Inflated monitors. The owner is an atomic word claimed by compare-and-swap, so entering and
//! leaving an uncontended or briefly contended monitor takes no lock and no system call; a
//! thread that cannot claim it spins within an adaptive time budget (as HotSpot's
//! `ObjectMonitor`), then blocks on the entry condition. Leaving wakes a blocked entrant only if
//! there is one. The wait set (`wait`/`notify` tickets) lives under the monitor's mutex.

use crate::thread::Thread;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::{Acquire, Relaxed, Release, SeqCst}};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The spin budget before blocking, in nanoseconds (adapted per monitor).
const MIN_SPIN: u32 = 500;
const MAX_SPIN: u32 = 20_000;

/// The wait set: waiters take consecutive tickets; `notify` releases the oldest ticket still
/// waiting and `notifyAll` every ticket issued so far, so a notification always goes to a
/// thread that was waiting when it was sent.
#[derive(Default)]
pub(super) struct Tickets {
    /// The next ticket to hand out, and every ticket below `released` may leave.
    pub next: u64,
    pub released: u64,
    /// Tickets whose waiter left on its own (timeout, interrupt) before being released.
    pub abandoned: Vec<u64>,
}

impl Tickets {
    /// Releases the oldest waiting ticket; false if nobody waits.
    pub fn release_one(&mut self) -> bool {
        while self.released < self.next {
            let t = self.released;
            self.released += 1;
            match self.abandoned.iter().position(|&a| a == t) {
                Some(i) => {
                    self.abandoned.swap_remove(i);
                }
                None => return true,
            }
        }
        false
    }
}

pub(super) struct Monitor {
    /// The owning thread's token, or 0.
    owner: AtomicU64,
    /// Recursion depth; only the owner reads or writes it.
    count: AtomicU32,
    /// Threads blocked (or about to block) on `entry`.
    entrants: AtomicU32,
    spin: AtomicU32,
    /// `Tickets::released`, readable without the lock (waiters spin on it).
    released: AtomicU64,
    /// The spin budget of waiters, adapted as `spin`.
    wait_spin: AtomicU32,
    tickets: Mutex<Tickets>,
    entry: Condvar,
    waiting: Condvar,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor {
            owner: AtomicU64::new(0),
            count: AtomicU32::new(0),
            entrants: AtomicU32::new(0),
            spin: AtomicU32::new(MIN_SPIN * 4),
            released: AtomicU64::new(0),
            wait_spin: AtomicU32::new(MIN_SPIN * 4),
            tickets: Mutex::new(Tickets::default()),
            entry: Condvar::new(),
            waiting: Condvar::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WaitError {
    NotOwner,
    Interrupted,
}

impl Monitor {
    fn lock(&self) -> MutexGuard<'_, Tickets> {
        self.tickets.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn claim(&self, me: u64) -> bool {
        self.owner.load(Relaxed) == 0 && self.owner.compare_exchange(0, me, SeqCst, Relaxed).is_ok()
    }

    pub fn owned_by(&self, me: u64) -> bool {
        self.owner.load(Acquire) == me
    }

    /// Hands the monitor to its thin-lock owner `me` at depth `count` (inflation by the owner).
    pub fn adopt(&self, me: u64, count: u32) {
        self.count.store(count, Relaxed);
        self.owner.store(me, SeqCst);
    }

    /// `o` is the monitor's object (reported to `ThreadInfo` while the caller is blocked).
    pub fn enter(&self, t: &Thread, me: u64, o: crate::heap::Oop) {
        if self.owner.load(Relaxed) == me {
            self.count.fetch_add(1, Relaxed);
            return;
        }
        if !self.claim(me) && !self.spin_claim(me) {
            super::BLOCKED_ENTERS.fetch_add(1, Relaxed);
            // Blocked outside Java so a collection can proceed; no Rust lock is held on return.
            crate::natives::block_on(t, o);
            crate::gc::safepoint::blocking(t, || drop(self.block_claim(self.lock(), me)));
            crate::natives::unblock(t);
        }
        self.count.store(1, Relaxed);
    }

    /// Spins for the monitor within the adaptive budget.
    fn spin_claim(&self, me: u64) -> bool {
        spin_until(&self.spin, || self.claim(me))
    }

    /// Blocks on `entry` until the monitor is claimed.
    fn block_claim<'a>(&'a self, mut g: MutexGuard<'a, Tickets>, me: u64) -> MutexGuard<'a, Tickets> {
        // Pairs with `release`: either this claim sees the owner gone, or the releaser sees
        // this entrant and wakes it.
        self.entrants.fetch_add(1, SeqCst);
        while !self.claim(me) {
            g = self.entry.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        self.entrants.fetch_sub(1, SeqCst);
        g
    }

    /// Clears the owner and wakes one blocked entrant, if any (`g`: the lock, if held).
    fn release(&self, g: Option<&MutexGuard<'_, Tickets>>) {
        self.owner.store(0, SeqCst);
        if self.entrants.load(SeqCst) != 0 {
            match g {
                Some(_) => self.entry.notify_one(),
                None => {
                    drop(self.lock());
                    self.entry.notify_one();
                }
            }
        }
    }

    /// Releases one level of ownership; false if `me` does not own the monitor.
    pub fn exit(&self, me: u64) -> bool {
        if self.owner.load(Relaxed) != me {
            return false;
        }
        let c = self.count.load(Relaxed);
        if c <= 1 {
            self.count.store(0, Relaxed);
            self.release(None);
        } else {
            self.count.store(c - 1, Relaxed);
        }
        true
    }

    /// `Object.wait` by the owner `me`, run blocked outside Java.
    pub fn wait(&self, me: u64, millis: i64, interrupted: &dyn Fn() -> bool) -> Result<(), WaitError> {
        let _resolution = crate::os::HighResolution::for_millis(millis.max(0) as u64);
        let mut g = self.lock();
        if self.owner.load(Relaxed) != me {
            return Err(WaitError::NotOwner);
        }
        let count = self.count.swap(0, Relaxed);
        let ticket = g.next;
        g.next += 1;
        self.release(Some(&g));
        // A notification usually follows soon (a hand-off): spin for it with the lock free.
        drop(g);
        spin_until(&self.wait_spin, || self.released.load(Acquire) > ticket);
        let mut g = self.lock();
        let deadline = (millis > 0).then(|| Instant::now() + Duration::from_millis(millis as u64));
        let mut was_interrupted = false;
        while ticket >= g.released {
            if interrupted() {
                was_interrupted = true;
                break;
            }
            match deadline {
                None => g = self.waiting.wait(g).unwrap_or_else(|e| e.into_inner()),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        break;
                    }
                    g = self.waiting.wait_timeout(g, d - now).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        }
        if ticket >= g.released {
            // Left without a notification: a later notify must go to another waiter.
            g.abandoned.push(ticket);
        }
        drop(g);
        if !self.claim(me) && !self.spin_claim(me) {
            drop(self.block_claim(self.lock(), me));
        }
        self.count.store(count, Relaxed);
        if was_interrupted {
            Err(WaitError::Interrupted)
        } else {
            Ok(())
        }
    }

    pub fn notify(&self, me: u64, all: bool) -> Result<(), WaitError> {
        if self.owner.load(Relaxed) != me {
            return Err(WaitError::NotOwner);
        }
        let mut g = self.lock();
        let woke = if all {
            g.abandoned.clear();
            let woke = g.released < g.next;
            g.released = g.next;
            woke
        } else {
            g.release_one()
        };
        self.released.store(g.released, Release);
        if woke {
            // Waiters share the condition variable and each checks its own ticket.
            self.waiting.notify_all();
        }
        Ok(())
    }

    /// Wakes the waiters so an interrupted one can notice (`Thread.interrupt`).
    pub fn wake_waiters(&self) {
        let _g = self.lock();
        self.waiting.notify_all();
    }

    #[cfg(test)]
    pub fn waiters(&self) -> u64 {
        self.lock().next
    }
}

/// Spins until `done`, within the adaptive time budget `spin` (doubled on success, halved on
/// failure). Returns `done`'s last result.
fn spin_until(spin: &AtomicU32, done: impl Fn() -> bool) -> bool {
    let budget = spin.load(Relaxed);
    let until = Instant::now() + Duration::from_nanos(budget as u64);
    loop {
        for _ in 0..32 {
            std::hint::spin_loop();
            if done() {
                spin.store((budget * 2).min(MAX_SPIN), Relaxed);
                return true;
            }
        }
        if Instant::now() >= until {
            spin.store((budget / 2).max(MIN_SPIN), Relaxed);
            return false;
        }
    }
}
