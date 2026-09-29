//! Java monitors (`synchronized`, `wait`/`notify`).
//!
//! Objects never move, so a monitor is found by the object's address in a sharded table. Each
//! monitor records its owner and recursion count; blocked entrants and waiters sleep on condition
//! variables. (Thin locks in the mark word are a later optimisation behind this interface.)

use crate::heap::Oop;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

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

pub fn enter(o: Oop) {
    let me = thread_token();
    let m = monitor_of(o);
    let mut s = m.state.lock().unwrap();
    while s.owner != 0 && s.owner != me {
        s = m.entry.wait(s).unwrap();
    }
    s.owner = me;
    s.count += 1;
}

/// Releases one level of ownership. Returns false if the caller does not own the monitor.
pub fn exit(o: Oop) -> bool {
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
    monitor_of(o).state.lock().unwrap().owner == thread_token()
}

#[derive(Debug, PartialEq, Eq)]
pub enum WaitError {
    NotOwner,
}

/// `Object.wait(millis)`; 0 waits until notified.
pub fn wait(o: Oop, millis: i64) -> Result<(), WaitError> {
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
    while s.notified == 0 {
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
    if s.notified > 0 {
        s.notified -= 1;
    }
    s.waiters -= 1;
    while s.owner != 0 {
        s = m.entry.wait(s).unwrap();
    }
    s.owner = me;
    s.count = count;
    Ok(())
}

pub fn notify(o: Oop, all: bool) -> Result<(), WaitError> {
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
        let o = 0x1000usize as Oop;
        enter(o);
        enter(o);
        assert!(holds_lock(o));
        assert!(exit(o));
        assert!(exit(o));
        assert!(!exit(o));
        assert_eq!(wait(o, 1), Err(WaitError::NotOwner));
    }

    #[test]
    fn notify_wakes_a_waiter() {
        let o = 0x2000usize as Oop;
        let addr = o as usize;
        let waiter = std::thread::spawn(move || {
            let o = addr as Oop;
            enter(o);
            wait(o, 0).unwrap();
            exit(o);
        });
        loop {
            enter(o);
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
