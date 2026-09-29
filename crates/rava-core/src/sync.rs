//! Java monitors (`synchronized`, `wait`/`notify`).
//!
//! Uncontended locking is a compare-and-swap on the object's mark word (a *thin* lock: owner
//! token and recursion count, next to the identity hash). A thin lock is *inflated* into a full
//! monitor — owner, count, condition variables, found by the object's address (objects never
//! move) — by its owner when it must wait, when recursion overflows, or when it was acquired
//! after contention. Contenders on a thin lock back off until it is released or inflated.
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
const THIN: u64 = 1;
const INFLATED: u64 = 2;
const COUNT_SHIFT: u32 = 2;
const COUNT_MASK: u64 = 0x3f << COUNT_SHIFT;
const OWNER_SHIFT: u32 = 40;
/// Bits a lock transition must preserve (the identity hash).
const HASH_BITS: u64 = 0xff_ffff_ff00;

fn mark(o: Oop) -> &'static AtomicU64 {
    // SAFETY: callers pass live objects.
    unsafe { obj::mark(o) }
}

const SHARDS: usize = 64;

#[derive(Default)]
struct State {
    owner: u64,
    count: u32,
    /// Pending notifications not yet consumed by a waiter.
    notified: u32,
    waiters: u32,
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

fn enter_inflated(t: &Thread, o: Oop, me: u64) {
    let m = monitor_of(o);
    let mut s = m.state.lock().unwrap();
    if s.owner != 0 && s.owner != me {
        // Blocked on another owner: let a collection proceed meanwhile.
        s = crate::gc::safepoint::blocking(t, || {
            while s.owner != 0 && s.owner != me {
                s = m.entry.wait(s).unwrap();
            }
            s
        });
    }
    s.owner = me;
    s.count += 1;
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
    s.waiters += 1;
    m.entry.notify_one();
    let deadline = (millis > 0).then(|| std::time::Instant::now() + Duration::from_millis(millis as u64));
    let mut was_interrupted = false;
    while s.notified == 0 {
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
    if s.notified > 0 && !was_interrupted {
        s.notified -= 1;
    }
    s.waiters -= 1;
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
    if s.waiters > s.notified {
        if all {
            s.notified = s.waiters;
            m.waiting.notify_all();
        } else {
            s.notified += 1;
            m.waiting.notify_one();
        }
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
        assert_eq!(wait(&t, o, 1), Err(WaitError::NotOwner));
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
            wait(&t, o, 0).unwrap();
            exit(o);
        });
        let t = Thread::new();
        loop {
            enter(&t, o);
            let waiting = monitor_of(o).state.lock().unwrap().waiters;
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
