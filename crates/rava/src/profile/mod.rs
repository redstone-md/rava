//! The recording profiler (`docs/profiling.md`): `RAVA_RECORD=<file>[,option...]` samples the
//! Java threads into one binary recording that `rava-prof` analyzes. Sampling is implemented
//! for Windows; the format and the analysis are portable.

pub mod format;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod recorder;

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static ON: AtomicBool = AtomicBool::new(false);
/// Bytes per allocation sample, 0 when the recording takes none (`alloc` option).
static ALLOC_STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// What `RAVA_RECORD` asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub path: std::path::PathBuf,
    /// Microseconds between samples of a thread.
    pub interval: u64,
    /// Samples blocked threads too (`mode=wall`), not only those that used the CPU.
    pub wall: bool,
    /// Bytes a thread allocates per allocation sample (`alloc[=<KiB>]`), 0 for none.
    pub alloc: usize,
}

impl Config {
    /// Parses `path[,interval=<µs>][,mode=cpu|wall][,alloc[=<KiB>]]`.
    pub fn parse(spec: &str) -> Result<Config, String> {
        let mut parts = spec.split(',');
        let path = parts.next().filter(|p| !p.is_empty()).ok_or("RAVA_RECORD needs a file path")?;
        let mut c = Config { path: path.into(), interval: 1000, wall: false, alloc: 0 };
        for opt in parts {
            if opt == "alloc" {
                c.alloc = 1 << 20;
                continue;
            }
            match opt.split_once('=') {
                Some(("alloc", v)) => c.alloc = v.parse::<usize>().ok().filter(|&k| k >= 16).ok_or_else(|| format!("RAVA_RECORD: alloc takes KiB, at least 16, not {v}"))? << 10,
                Some(("interval", v)) => c.interval = v.parse().ok().filter(|&v: &u64| v >= 100).ok_or_else(|| format!("RAVA_RECORD: interval must be at least 100 µs, not {v}"))?,
                Some(("mode", "cpu")) => c.wall = false,
                Some(("mode", "wall")) => c.wall = true,
                _ => return Err(format!("RAVA_RECORD: unknown option {opt:?}")),
            }
        }
        Ok(c)
    }
}

/// Starts recording when `RAVA_RECORD` is set (from the launcher, before any Java thread).
pub fn start() {
    let Some(spec) = std::env::var_os("RAVA_RECORD") else { return };
    let config = match Config::parse(&spec.to_string_lossy()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rava: {e}");
            return;
        }
    };
    #[cfg(windows)]
    {
        let step = config.alloc;
        if recorder::start(config) {
            ALLOC_STEP.store(step, Relaxed);
            ON.store(true, Relaxed);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = config;
        eprintln!("rava: RAVA_RECORD samples on Windows only so far");
    }
}

/// Ends the recording before the process exits: the recorder writes each thread's CPU once
/// more and flushes (waiting at most two seconds for it).
pub fn finish() {
    if ON.load(Relaxed) {
        #[cfg(windows)]
        recorder::finish();
    }
}

/// Registers the calling thread (its `Thread`) for sampling.
pub fn attach_current(thread: usize) {
    if ON.load(Relaxed) {
        #[cfg(windows)]
        capture::attach_current(thread);
    }
    let _ = thread;
}

/// Unregisters the calling thread before it exits.
pub fn detach_current() {
    if ON.load(Relaxed) {
        #[cfg(windows)]
        capture::detach_current();
    }
}

thread_local! {
    /// The class the calling thread allocates an instance of (`allocating`).
    static ALLOC_CLASS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Bytes the calling thread allocated since its last allocation sample.
    static ALLOC_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}



/// Notes that the calling thread allocates an object of class `k` (the runtime's allocation
/// entry points, which compiled code reaches when its bump run is exhausted).
#[inline]
pub fn allocating(k: &'static crate::klass::Klass) {
    if ALLOC_STEP.load(Relaxed) != 0 {
        ALLOC_CLASS.with(|c| c.set(k as *const _ as usize));
    }
}

/// Thread `t` filled a run of `bytes` (or allocated one large object of that size) and is
/// allocating an object of the class `allocating` noted: once a megabyte has gone so since the
/// last (the `alloc` option's step), an allocation sample weighing those bytes, at this object's
/// class and stack.
pub fn allocated(t: &crate::thread::Thread, bytes: usize) {
    let step = ALLOC_STEP.load(Relaxed);
    if step == 0 || bytes == 0 {
        return;
    }
    let total = ALLOC_BYTES.with(|b| b.get()) + bytes;
    if total < step {
        ALLOC_BYTES.with(|b| b.set(total));
        return;
    }
    ALLOC_BYTES.with(|b| b.set(0));
    let class = ALLOC_CLASS.with(|c| c.get());
    #[cfg(windows)]
    capture::allocation(t, class, total);
    let _ = (t, class);
}

/// Whether a recording is being made (the compilers then keep what it needs).
pub fn recording() -> bool {
    ON.load(Relaxed)
}

/// The methods inlined into optimized code at `start`: from each offset on, the frames there,
/// innermost first, each a method and bytecode index (the compiled method last).
pub fn code_scopes(start: usize, scopes: Vec<(u32, Vec<(&'static crate::method::Method, i32)>)>) {
    #[cfg(windows)]
    if ON.load(Relaxed) {
        recorder::code_scopes(start, scopes);
    }
    #[cfg(not(windows))]
    let _ = (start, scopes);
}

/// Compiled code installed at `start` for `size` bytes, named `method [tier]`.
pub fn code_installed(start: usize, size: usize, name: &str) {
    if ON.load(Relaxed) {
        #[cfg(windows)]
        recorder::code_installed(start, size, name);
    }
    let _ = (start, size, name);
}

/// Splits a code name `holder.name(desc) [tier]` into the method and its tier.
pub fn split_code_name(name: &str) -> (&str, format::Tier) {
    use format::Tier;
    match name.rsplit_once(" [") {
        Some((m, t)) => {
            let tier = match t.trim_end_matches(']') {
                "opt" => Tier::Opt,
                "opt osr" => Tier::Osr,
                t if t.starts_with('b') => Tier::Baseline,
                _ => Tier::Unknown,
            };
            (m, tier)
        }
        None => (name, Tier::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use format::Tier;

    #[test]
    fn parses_the_specification() {
        assert_eq!(Config::parse("a.rprof").unwrap(), Config { path: "a.rprof".into(), interval: 1000, wall: false, alloc: 0 });
        assert_eq!(Config::parse("a.rprof,interval=500,mode=wall").unwrap(), Config { path: "a.rprof".into(), interval: 500, wall: true, alloc: 0 });
        assert_eq!(Config::parse("a,alloc").unwrap().alloc, 1 << 20);
        assert_eq!(Config::parse("a,alloc=256").unwrap().alloc, 256 << 10);
        assert!(Config::parse("a,alloc=1").is_err());
        assert!(Config::parse("").is_err());
        assert!(Config::parse("a,interval=5").is_err());
        assert!(Config::parse("a,colour=red").is_err());
    }

    #[test]
    fn splits_code_names() {
        assert_eq!(split_code_name("java/lang/String.hashCode()I [opt]"), ("java/lang/String.hashCode()I", Tier::Opt));
        assert_eq!(split_code_name("A.run()V [opt osr]"), ("A.run()V", Tier::Osr));
        assert_eq!(split_code_name("A.run()V [b1]"), ("A.run()V", Tier::Baseline));
        assert_eq!(split_code_name("stub"), ("stub", Tier::Unknown));
    }
}
