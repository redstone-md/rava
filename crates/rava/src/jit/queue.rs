//! Background compilation (HotSpot's compile broker): methods that got hot are queued and
//! compiled by compiler threads while the thread that asked keeps running its current code
//! (interpreted, baseline or optimized), which the new code then replaces for later calls.
//! `RAVA_SYNC_COMPILE=1` compiles on the requesting thread instead.

use crate::method::Method;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};

pub(super) enum Job {
    /// First compilation (baseline tier 1).
    Baseline(&'static Method),
    /// Hot baseline code: recompile at tier 2.
    TierUp(&'static Method),
    /// Optimized code that keeps deoptimizing.
    Recompile(&'static Method),
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

fn background() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SYNC_COMPILE").is_none())
}

fn queue() -> &'static Queue {
    static Q: OnceLock<Queue> = OnceLock::new();
    Q.get_or_init(|| {
        let workers = std::env::var("RAVA_COMPILER_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
            (cores / 2).clamp(1, 3)
        });
        for i in 0..workers {
            std::thread::Builder::new().name(format!("rava-compiler-{i}")).stack_size(64 << 20).spawn(work).expect("spawn a compiler thread");
        }
        Queue { jobs: Mutex::new(VecDeque::new()), ready: Condvar::new() }
    })
}

/// Queues `job` (or runs it now when compiling synchronously).
pub(super) fn submit(job: Job) {
    if !background() {
        return run(job);
    }
    let q = queue();
    q.jobs.lock().unwrap().push_back(job);
    q.ready.notify_one();
}

fn work() {
    let q = queue();
    loop {
        let job = {
            let mut jobs = q.jobs.lock().unwrap();
            loop {
                match jobs.pop_front() {
                    Some(j) => break j,
                    None => jobs = q.ready.wait(jobs).unwrap(),
                }
            }
        };
        run(job);
    }
}

fn run(job: Job) {
    match job {
        Job::Baseline(m) => {
            if m.compiled().is_none() {
                super::compile_now(m);
            }
        }
        Job::TierUp(m) => super::tier::compile_tier2(m),
        Job::Recompile(m) => super::opt::recompile(m),
    }
}
