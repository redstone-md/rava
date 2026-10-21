//! The collector's helper threads: started once (named `rava-gc-N`) and parked between
//! collections, instead of a fresh set spawned for every collection. Spawning put thread
//! creation and start-up latency inside each pause (once per helper, before the first barrier
//! of the mark could pass), which grows with the core count.

use std::sync::{Condvar, Mutex, OnceLock};

/// The job of the current collection, borrowed for its duration (see `run`).
type Job = &'static (dyn Fn() + Sync);

#[derive(Default)]
struct State {
    job: Option<Job>,
    /// Bumped for every job; helpers run each epoch's job at most once.
    epoch: u64,
    /// Helpers with an index below this run the current job.
    wanted: usize,
    /// Helpers still running the current job.
    running: usize,
    spawned: usize,
}

#[derive(Default)]
struct Pool {
    state: Mutex<State>,
    go: Condvar,
    done: Condvar,
}

fn pool() -> &'static Pool {
    static P: OnceLock<Pool> = OnceLock::new();
    P.get_or_init(Pool::default)
}

/// Runs `job` on the calling thread and on `helpers` helper threads at once, returning when
/// every copy has returned.
pub(super) fn run(helpers: usize, job: &(dyn Fn() + Sync)) {
    if helpers == 0 {
        return job();
    }
    let p = pool();
    {
        let mut s = p.state.lock().unwrap();
        while s.spawned < helpers {
            let index = s.spawned;
            std::thread::Builder::new().name(format!("rava-gc-{index}")).spawn(move || helper(index)).expect("spawn a GC thread");
            s.spawned += 1;
        }
        // SAFETY: the job outlives its use: this function waits below until no helper runs it,
        // and helpers drop it (`job` is cleared) before `run` returns.
        s.job = Some(unsafe { std::mem::transmute::<&(dyn Fn() + Sync), Job>(job) });
        s.epoch += 1;
        s.wanted = helpers;
        s.running = helpers;
        p.go.notify_all();
    }
    job();
    let mut s = p.state.lock().unwrap();
    while s.running > 0 {
        s = p.done.wait(s).unwrap();
    }
    s.job = None;
}

fn helper(index: usize) {
    // Every Java thread waits for a collection: its helpers run ahead of other work.
    crate::os::vm_thread_priority();
    // A VM thread, sampled as such by a recording (no Java frames).
    crate::profile::attach_current(0);
    let p = pool();
    let mut seen = 0;
    let mut s = p.state.lock().unwrap();
    loop {
        while s.epoch == seen {
            s = p.go.wait(s).unwrap();
        }
        seen = s.epoch;
        if index >= s.wanted {
            continue;
        }
        let job = s.job.expect("a job for the epoch");
        drop(s);
        job();
        s = p.state.lock().unwrap();
        s.running -= 1;
        if s.running == 0 {
            p.done.notify_one();
        }
    }
}
