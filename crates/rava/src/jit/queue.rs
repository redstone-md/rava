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
    /// Optimized code with call sites that were cold when compiled and now run.
    Reprofile(&'static Method),
    /// A loop running first-tier code in a method with optimized code: compile a version
    /// entered from it (`opt/osr.rs`).
    Osr(&'static Method, &'static super::OsrCode),
}

struct Queue {
    jobs: Mutex<VecDeque<Queued>>,
    ready: Condvar,
}

/// A job with when it was queued and its method's invocation and back-edge counts then.
struct Queued {
    job: Job,
    at: std::time::Instant,
    events: u64,
}

impl Job {
    fn method(&self) -> &'static Method {
        match *self {
            Job::Baseline(m) | Job::TierUp(m) | Job::Recompile(m) | Job::Reprofile(m) | Job::Osr(m, _) => m,
        }
    }

    /// Whether the job goes before any by hotness: a loop waiting for its OSR version, or a
    /// method that kept deoptimizing and runs baseline code until it is recompiled.
    fn urgent(&self) -> bool {
        matches!(self, Job::Osr(..) | Job::Recompile(_))
    }
}

/// Invocations and back-edges `m` has counted (lower tiers keep counting while it waits).
fn events(m: &'static Method) -> u64 {
    use std::sync::atomic::Ordering::Relaxed;
    m.invocations.load(Relaxed) as u64 + m.backedges.load(Relaxed) as u64
}

/// Takes the job to compile next (HotSpot's `select_task`): the first urgent one, else the
/// one whose method counted the most events per millisecond since it was queued, plus a
/// little for each second waited so that none starves.
fn select(jobs: &mut VecDeque<Queued>) -> Option<Queued> {
    if let Some(i) = jobs.iter().position(|q| q.job.urgent()) {
        return jobs.remove(i);
    }
    let score = |q: &Queued| {
        let ms = q.at.elapsed().as_secs_f64() * 1e3;
        events(q.job.method()).saturating_sub(q.events) as f64 / (ms + 1.0) + ms / 1000.0
    };
    let best = (0..jobs.len()).max_by(|&a, &b| score(&jobs[a]).total_cmp(&score(&jobs[b])))?;
    jobs.remove(best)
}

fn background() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SYNC_COMPILE").is_none())
}

fn queue() -> &'static Queue {
    static Q: OnceLock<Queue> = OnceLock::new();
    Q.get_or_init(|| {
        // A compiler thread per core, at a lower priority: compilation takes the cores the
        // application leaves idle (most of a server start runs on one or two threads) without
        // competing with its threads when they are busy.
        let workers = std::env::var("RAVA_COMPILER_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| crate::processors().clamp(2, 8));
        let nice = std::env::var("RAVA_COMPILER_NICE").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
        for i in 0..workers {
            std::thread::Builder::new().name(format!("rava-compiler-{i}")).stack_size(64 << 20).spawn(move || {
                if nice != 0 {
                    // SAFETY: plain syscalls on the calling thread.
                    crate::os::lower_priority(nice);
                }
                work()
            }).expect("spawn a compiler thread");
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
    let queued = Queued { events: events(job.method()), job, at: std::time::Instant::now() };
    q.jobs.lock().unwrap().push_back(queued);
    q.ready.notify_one();
}

fn work() {
    let q = queue();
    loop {
        let job = {
            let mut jobs = q.jobs.lock().unwrap();
            loop {
                match select(&mut jobs) {
                    Some(q) => break q.job,
                    None => jobs = q.ready.wait(jobs).unwrap(),
                }
            }
        };
        run(job);
    }
}

/// Nanoseconds spent compiling (`RAVA_PROFILE`).
pub static COMPILE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn run(job: Job) {
    let start = std::env::var_os("RAVA_PROFILE").is_some().then(std::time::Instant::now);
    match job {
        Job::Baseline(m) => {
            if m.compiled().is_none() {
                super::compile_now(m);
            }
        }
        Job::TierUp(m) => super::tier::compile_tier2(m),
        Job::Recompile(m) => super::opt::recompile(m),
        Job::Reprofile(m) => super::opt::reprofile(m),
        Job::Osr(m, c) => super::opt::osr::compile(m, c),
    }
    if let Some(start) = start {
        COMPILE_NS.fetch_add(start.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    }
}
