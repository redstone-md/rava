//! The idle collector: a VM thread that looks once a second whether the program has gone idle
//! and, once per idle spell, has the heap collected and given back (`Heap::collect_if_idle`).
//! An idle program allocates too little to reach the allocation slow path that would decide it
//! otherwise. `RAVA_IDLE_GC=0` turns it off.

use crate::thread::Thread;

pub fn start() {
    if std::env::var("RAVA_IDLE_GC").is_ok_and(|v| v == "0") {
        return;
    }
    let _ = std::thread::Builder::new().name("rava-idle-gc".into()).spawn(|| {
        let mut t = Thread::new();
        t.attach();
        loop {
            // Asleep in a safe state: collections do not wait for this thread.
            super::safepoint::blocking(&t, || std::thread::sleep(std::time::Duration::from_secs(1)));
            crate::vm::vm().heap.collect_if_idle(&mut t);
        }
    });
}
