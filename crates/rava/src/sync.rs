//! Java monitors (`synchronized`, `wait`/`notify`).
//!
//! Uncontended locking is a compare-and-swap on the object's mark word (a *thin* lock: owner
//! token and recursion count, next to the identity hash). A thin lock is *inflated* into a full
//! monitor — owner, count, condition variables, found by the object's address (the collector
//! never moves an object whose mark word is locked or inflated) — by its owner when it must
//! wait, when recursion overflows, or when it was acquired after contention. Contenders on a thin lock back off until it is released or inflated.
//!
//! Mark word: bits 0-1 lock state, 2-7 recursion count - 1, 8-38 identity hash, 40-63 owner.

use crate::heap::{obj, Oop};
use crate::thread::Thread;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const STATE_MASK: u64 = 0b11;
const UNLOCKED: u64 = 0;
pub(crate) const THIN: u64 = 1;
const INFLATED: u64 = 2;
const COUNT_SHIFT: u32 = 2;
const COUNT_MASK: u64 = 0x3f << COUNT_SHIFT;
pub(crate) const OWNER_SHIFT: u32 = 40;
/// Bits a lock transition must preserve (the identity hash).
pub(crate) const HASH_BITS: u64 = 0xff_ffff_ff00;

fn mark(o: Oop) -> &'static AtomicU64 {
    // SAFETY: callers pass live objects.
    unsafe { obj::mark(o) }
}

/// Contention counters (read under `RAVA_PROFILE`): monitors inflated, thin-lock back-off
/// sleeps, and inflated enters that actually blocked on another owner.
pub static INFLATIONS: AtomicU64 = AtomicU64::new(0);
pub static THIN_SLEEPS: AtomicU64 = AtomicU64::new(0);
pub static BLOCKED_ENTERS: AtomicU64 = AtomicU64::new(0);

const SHARDS: usize = 64;

/// A monitor's owner and wait set. Waiters take consecutive tickets; `notify` releases the
/// oldest ticket still waiting and `notifyAll` every ticket issued so far, so a notification
/// always goes to a thread that was waiting when it was sent.
#[derive(Default)]
struct State {
    owner: u64,
    count: u32,
    /// The next ticket to hand out, and every ticket below `released` may leave.
    next_ticket: u64,
    released: u64,
    /// Tickets whose waiter left on its own (timeout, interrupt) before being released.
    abandoned: Vec<u64>,
}

impl State {
    /// Releases the oldest waiting ticket; false if nobody waits.
    fn release_one(&mut self) -> bool {
        while self.released < self.next_ticket {
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

#[derive(Default)]
struct Monitor {
    state: Mutex<State>,
    entry: Condvar,
    waiting: Condvar,
}

struct Table {
    shards: Vec<Mutex<HashMap<usize, Arc<Monitor>>>>,
}

fn table() -> &'static Table {
    static T: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    T.get_or_init(|| Table { shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect() })
}

fn monitor_of(o: Oop) -> Arc<Monitor> {
    let key = o as usize;
    let shard = &table().shards[(key >> 4) % SHARDS];
    shard.lock().unwrap().entry(key).or_default().clone()
}

/// A process-unique id for the calling OS thread.
pub fn thread_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local!(static ID: u64 = NEXT.fetch_add(1, Ordering::Relaxed));
    ID.with(|id| *id)
}

pub fn enter(t: &Thread, o: Oop) {
    let me = thread_token();
    let word = mark(o);
    let mut spins = 0u32;
    loop {
        let m = word.load(Ordering::Relaxed);
        match m & STATE_MASK {
            UNLOCKED => {
                let locked = (m & HASH_BITS) | THIN | me << OWNER_SHIFT;
                if word.compare_exchange_weak(m, locked, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    if spins > 0 {
                        // Contended: inflate so later contenders block instead of spinning.
                        inflate_owned(o);
                    }
                    return;
                }
            }
            THIN if m >> OWNER_SHIFT == me => {
                if m & COUNT_MASK == COUNT_MASK {
                    inflate_owned(o);
                    continue;
                }
                if word.compare_exchange_weak(m, m + (1 << COUNT_SHIFT), Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                    return;
                }
            }
            THIN => {
                spins += 1;
                back_off(t, spins);
            }
            _ => return enter_inflated(t, o, me),
        }
    }
}

/// Waiting for another thread's thin lock: spin briefly, then yield, then sleep (in the native
/// state, so a collection can proceed).
fn back_off(t: &Thread, spins: u32) {
    if spins < 64 {
        std::hint::spin_loop();
    } else if spins < 256 {
        std::thread::yield_now();
    } else {
        THIN_SLEEPS.fetch_add(1, Ordering::Relaxed);
        crate::gc::safepoint::blocking(t, || std::thread::sleep(Duration::from_micros(100)));
    }
}

/// Converts the caller's thin lock on `o` into a full monitor with the same count.
fn inflate_owned(o: Oop) {
    let word = mark(o);
    let m = word.load(Ordering::Relaxed);
    if m & STATE_MASK != THIN {
        return;
    }
    INFLATIONS.fetch_add(1, Ordering::Relaxed);
    let mon = monitor_of(o);
    {
        let mut s = mon.state.lock().unwrap();
        s.owner = m >> OWNER_SHIFT;
        s.count = ((m & COUNT_MASK) >> COUNT_SHIFT) as u32 + 1;
    }
    // Only the owner changes a thin mark (others only read it), so a plain store suffices; the
    // hash may have been set concurrently, so keep the latest hash bits.
    loop {
        let cur = word.load(Ordering::Relaxed);
        if word.compare_exchange_weak(cur, (cur & HASH_BITS) | INFLATED, Ordering::Release, Ordering::Relaxed).is_ok() {
            return;
        }
    }
}

/// Times an inflated enter spin-checks the owner before parking. A contended monitor is usually
/// held only briefly, so a short spin often claims it without the cost of a park/unpark; past
/// this the owner is holding the monitor long enough that blocking is cheaper than spinning.
const ENTER_SPINS: u32 = 40;

fn enter_inflated(t: &Thread, o: Oop, me: u64) {
    let m = monitor_of(o);
    {
        let mut s = m.state.lock().unwrap();
        if s.owner == 0 || s.owner == me {
            s.owner = me;
            s.count += 1;
            return;
        }
    }
    // Adaptive spin before parking: retry the claim a bounded number of times, pausing each
    // round. This turns briefly held monitors into a spin rather than an expensive condition-
    // variable wait, which matters when many threads share a few hot monitors. The claim uses
    // `try_lock`, so a spinner never blocks the owner from taking the lock to release.
    for _ in 0..ENTER_SPINS {
        std::hint::spin_loop();
        if let Ok(mut s) = m.state.try_lock() {
            if s.owner == 0 || s.owner == me {
                s.owner = me;
                s.count += 1;
                return;
            }
        }
    }
    // Blocked on another owner: wait in the native state so a collection can proceed. The
    // monitor is claimed, and the Rust lock released, before leaving it: a thread stopped at a
    // safepoint must never hold a lock a running thread could be waiting for.
    BLOCKED_ENTERS.fetch_add(1, Ordering::Relaxed);
    crate::gc::safepoint::blocking(t, || {
        let mut s = m.state.lock().unwrap();
        while s.owner != 0 && s.owner != me {
            s = m.entry.wait(s).unwrap();
        }
        s.owner = me;
        s.count += 1;
    });
}

/// Releases one level of ownership. Returns false if the caller does not own the monitor.
pub fn exit(o: Oop) -> bool {
    let word = mark(o);
    let me = thread_token();
    loop {
        let m = word.load(Ordering::Relaxed);
        match m & STATE_MASK {
            THIN if m >> OWNER_SHIFT == me => {
                let next = if m & COUNT_MASK == 0 { m & HASH_BITS } else { m - (1 << COUNT_SHIFT) };
                if word.compare_exchange_weak(m, next, Ordering::Release, Ordering::Relaxed).is_ok() {
                    return true;
                }
            }
            INFLATED => return exit_inflated(o),
            _ => return false,
        }
    }
}

fn exit_inflated(o: Oop) -> bool {
    let m = monitor_of(o);
    let mut s = m.state.lock().unwrap();
    if s.owner != thread_token() || s.count == 0 {
        return false;
    }
    s.count -= 1;
    if s.count == 0 {
        s.owner = 0;
        m.entry.notify_one();
    }
    true
}

pub fn holds_lock(o: Oop) -> bool {
    let m = mark(o).load(Ordering::Relaxed);
    match m & STATE_MASK {
        THIN => m >> OWNER_SHIFT == thread_token(),
        INFLATED => monitor_of(o).state.lock().unwrap().owner == thread_token(),
        _ => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WaitError {
    NotOwner,
    Interrupted,
}

/// `Object.wait(millis)`; 0 waits until notified. The thread is blocked outside Java meanwhile.
/// `interrupted` reads (and on true, clears) the thread's interrupt status; an interrupt before
/// or during the wait ends it with `Interrupted` once the monitor is re-acquired.
pub fn wait(t: &Thread, o: Oop, millis: i64, interrupted: &dyn Fn() -> bool) -> Result<(), WaitError> {
    if !holds_lock(o) {
        return Err(WaitError::NotOwner);
    }
    if interrupted() {
        return Err(WaitError::Interrupted);
    }
    inflate_owned(o);
    let rec = t.gc_record;
    rec.inspect(|r| r.waiting_on.store(o as usize, Ordering::SeqCst));
    let r = crate::gc::safepoint::blocking(t, || wait_blocking(o, millis, interrupted));
    rec.inspect(|r| r.waiting_on.store(0, Ordering::SeqCst));
    r
}

/// Wakes the waiters on `o`'s monitor so an interrupted one can notice (`Thread.interrupt`).
pub fn wake_waiters(o: usize) {
    let m = monitor_of(o as Oop);
    let _s = m.state.lock().unwrap();
    m.waiting.notify_all();
}

fn wait_blocking(o: Oop, millis: i64, interrupted: &dyn Fn() -> bool) -> Result<(), WaitError> {
    let me = thread_token();
    let m = monitor_of(o);
    let mut s = m.state.lock().unwrap();
    if s.owner != me {
        return Err(WaitError::NotOwner);
    }
    let count = std::mem::replace(&mut s.count, 0);
    s.owner = 0;
    let ticket = s.next_ticket;
    s.next_ticket += 1;
    m.entry.notify_one();
    let deadline = (millis > 0).then(|| std::time::Instant::now() + Duration::from_millis(millis as u64));
    let mut was_interrupted = false;
    while ticket >= s.released {
        if interrupted() {
            was_interrupted = true;
            break;
        }
        match deadline {
            None => s = m.waiting.wait(s).unwrap(),
            Some(d) => {
                let now = std::time::Instant::now();
                if now >= d {
                    break;
                }
                s = m.waiting.wait_timeout(s, d - now).unwrap().0;
            }
        }
    }
    if ticket >= s.released {
        // Left without a notification: a later notify must go to another waiter.
        s.abandoned.push(ticket);
    }
    while s.owner != 0 {
        s = m.entry.wait(s).unwrap();
    }
    s.owner = me;
    s.count = count;
    if was_interrupted {
        Err(WaitError::Interrupted)
    } else {
        Ok(())
    }
}

pub fn notify(o: Oop, all: bool) -> Result<(), WaitError> {
    let word = mark(o).load(Ordering::Relaxed);
    if word & STATE_MASK == THIN {
        // Waiters always inflate first, so a thin lock has nobody to notify.
        return if word >> OWNER_SHIFT == thread_token() { Ok(()) } else { Err(WaitError::NotOwner) };
    }
    if word & STATE_MASK == UNLOCKED {
        return Err(WaitError::NotOwner);
    }
    let m = monitor_of(o);
    let mut s = m.state.lock().unwrap();
    if s.owner != thread_token() {
        return Err(WaitError::NotOwner);
    }
    let woke = if all {
        s.abandoned.clear();
        let woke = s.released < s.next_ticket;
        s.released = s.next_ticket;
        woke
    } else {
        s.release_one()
    };
    if woke {
        // Waiters share the condition variable and each checks its own ticket.
        m.waiting.notify_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reentrant_enter_exit_and_ownership() {
        let t = Thread::new();
        let header = Box::leak(Box::new([0u64; 2]));
        let o = header.as_mut_ptr() as Oop;
        enter(&t, o);
        enter(&t, o);
        assert!(holds_lock(o));
        assert!(exit(o));
        assert!(exit(o));
        assert!(!exit(o));
        assert_eq!(wait(&t, o, 1, &|| false), Err(WaitError::NotOwner));
    }

    #[test]
    fn notify_releases_the_oldest_ticket_still_waiting() {
        // Tickets 0, 1 and 2 wait; 1 timed out.
        let mut s = State { next_ticket: 3, abandoned: vec![1], ..State::default() };
        assert!(s.release_one());
        assert_eq!(s.released, 1);
        assert!(s.release_one());
        assert_eq!(s.released, 3);
        assert!(!s.release_one());
    }

    #[test]
    fn notify_wakes_a_waiter() {
        let header = Box::leak(Box::new([0u64; 2]));
        let o = header.as_mut_ptr() as Oop;
        let addr = o as usize;
        let waiter = std::thread::spawn(move || {
            let t = Thread::new();
            let o = addr as Oop;
            enter(&t, o);
            wait(&t, o, 0, &|| false).unwrap();
            exit(o);
        });
        let t = Thread::new();
        loop {
            enter(&t, o);
            let waiting = monitor_of(o).state.lock().unwrap().next_ticket;
            if waiting == 1 {
                notify(o, false).unwrap();
                exit(o);
                break;
            }
            exit(o);
            std::thread::yield_now();
        }
        waiter.join().unwrap();
    }
}
