//! A sampled profile of the VM's live allocations (`RAVA_NATIVE_PROF=<KiB>`, with `RAVA_MEM_LOG`):
//! after about that many KiB a thread allocates, the next allocation records its native call
//! stack; a record goes when its block is freed. Each `RAVA_MEM_LOG` report then lists the
//! stacks holding the most live bytes, as offsets into the executable (resolve them with its
//! symbols, a `release-diag` build). Off, it costs nothing beyond `RAVA_MEM_LOG`'s counting.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;

const DEPTH: usize = 14;
const SHARDS: usize = 64;

/// The sampling interval in bytes (0: off).
static INTERVAL: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Bytes this thread allocates before its next sample.
    static COUNTDOWN: Cell<isize> = const { Cell::new(0) };
    /// Set while this thread works on the profile, whose own allocations are not profiled.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy)]
struct Sample {
    weight: usize,
    frames: [usize; DEPTH],
}

static TABLE: [Mutex<Option<HashMap<usize, Sample>>>; SHARDS] = [const { Mutex::new(None) }; SHARDS];

fn shard(p: usize) -> &'static Mutex<Option<HashMap<usize, Sample>>> {
    &TABLE[(p >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58]
}

/// Turns sampling on when `RAVA_NATIVE_PROF` asks for it.
pub fn init() {
    if let Some(kib) = std::env::var("RAVA_NATIVE_PROF").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&k| k > 0) {
        INTERVAL.store(kib << 10, Relaxed);
    }
}

/// Runs `f` with this thread's allocations left out of the profile; nothing if it already is.
fn guarded(f: impl FnOnce()) {
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    f();
    BUSY.with(|b| b.set(false));
}

/// Block `p` of `size` bytes was allocated.
#[inline]
pub fn allocated(p: usize, size: usize) {
    let interval = INTERVAL.load(Relaxed);
    if interval == 0 {
        return;
    }
    let left = COUNTDOWN.with(|c| {
        // A new thread starts at a random point of the interval: starting at zero sampled the
        // first allocation of every thread, which put the GC workers' first page tables at the
        // top of the profile.
        let start = if c.get() == 0 { (p.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) % interval + 1 } else { 0 };
        let left = c.get() + start as isize - size as isize;
        // After a sample the countdown starts over: carrying a large block's overshoot sampled
        // the next small ones too, each weighted as a whole interval.
        c.set(if left <= 0 { interval as isize } else { left });
        left
    });
    if left > 0 {
        return;
    }
    guarded(|| {
        let mut frames = [0usize; DEPTH];
        crate::os::native_frames(&mut frames);
        let sample = Sample { weight: size.max(interval), frames };
        shard(p).lock().unwrap().get_or_insert_with(HashMap::new).insert(p, sample);
    });
}

/// Block `p` is being freed.
#[inline]
pub fn freed(p: usize) {
    if INTERVAL.load(Relaxed) == 0 {
        return;
    }
    guarded(|| {
        if let Some(map) = shard(p).lock().unwrap().as_mut() {
            map.remove(&p);
        }
    });
}

/// The stacks holding the most sampled live bytes, one line each.
pub fn report() -> Vec<String> {
    if INTERVAL.load(Relaxed) == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    guarded(|| {
        let base = crate::os::exe_base();
        let mut by_stack: HashMap<[usize; DEPTH], (usize, usize)> = HashMap::new();
        for s in &TABLE {
            if let Some(map) = s.lock().unwrap().as_ref() {
                for sample in map.values() {
                    let e = by_stack.entry(sample.frames).or_default();
                    e.0 += sample.weight;
                    e.1 += 1;
                }
            }
        }
        let total: usize = by_stack.values().map(|v| v.0).sum();
        let mut top: Vec<_> = by_stack.into_iter().collect();
        top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        lines.push(format!("[native-prof] {} MiB live in sampled stacks", total >> 20));
        for (frames, (bytes, n)) in top.into_iter().take(400) {
            let offsets: Vec<String> = frames.iter().filter(|&&f| f != 0).map(|&f| format!("{:x}", f.wrapping_sub(base))).collect();
            lines.push(format!("[native-prof] {:>7} KiB {:>6} {}", bytes >> 10, n, offsets.join(" ")));
        }
    });
    lines
}
