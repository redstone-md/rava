//! `sun.management.ThreadImpl`, the natives behind `ThreadMXBean`: thread lists, `ThreadInfo`
//! with stacks (profilers sample with it), and the switches for statistics this VM does not
//! keep (per-thread CPU time and allocation, contention times), which report as unsupported.
//! Which monitors and synchronizers a thread holds is not tracked: dumps asking for them list
//! none.
//!
//! Stacks of other threads are copied with the world stopped, as the thread dump takes them
//! (`crate::dump`); the Java objects are made once it runs again.

use super::{ret_long, ret_ref, ret_void, NativeRegistry};
use crate::gc::safepoint::{self, Record};
use crate::heap::{obj, Oop};
use crate::method::Method;
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};
use crate::vm::vm;
use std::sync::atomic::Ordering::SeqCst;

const C: &str = "sun/management/ThreadImpl";
const INFO: &str = "java/lang/management/ThreadInfo";

vm_field!(tid_offset, "java/lang/Thread", "tid", "J");

/// `Thread.State` values of `JavaThreadStatus` (`thread.rs`) for the lock a thread waits on.
const IN_OBJECT_WAIT: [i32; 2] = [super::thread::STATUS_IN_OBJECT_WAIT, super::thread::STATUS_IN_OBJECT_WAIT_TIMED];

/// What a sample of one thread holds: its Java thread, state, the monitor it waits on and its
/// innermost frames.
struct Sample {
    thread: Oop,
    state: i32,
    lock: Oop,
    frames: Vec<(&'static Method, i32)>,
}

fn tid(jt: Oop) -> i64 {
    // SAFETY: a live java.lang.Thread.
    unsafe { obj::read::<i64>(jt, tid_offset()) }
}

/// Samples of the threads with the given ids (all threads for None), up to `depth` frames each
/// (all for a negative depth), in the order of `ids`; None where a thread is gone.
fn sample(t: &mut Thread, ids: Option<&[i64]>, depth: i32) -> Vec<Option<Sample>> {
    let limit = if depth < 0 { usize::MAX } else { depth as usize };
    loop {
        let taken = safepoint::stop_the_world(t, |_| {
            let threads: Vec<(&'static Record, Oop)> = safepoint::all_threads()
                .into_iter()
                .filter_map(|r| {
                    let th = r.thread.load(SeqCst);
                    // SAFETY: a registered thread's `Thread` lives while the world is stopped.
                    let jt = if th.is_null() { std::ptr::null_mut() } else { unsafe { (*th).java_thread } };
                    (!jt.is_null()).then_some((r, jt))
                })
                .collect();
            let take = |r: &'static Record, jt: Oop| {
                // SAFETY: as above; a stopped or native thread's frames hold still.
                let frames: Vec<_> = unsafe { (*r.thread.load(SeqCst)).frames() }.take(limit).collect();
                let state = super::thread::status_of(jt);
                let lock = if IN_OBJECT_WAIT.contains(&state) {
                    r.waiting_on.load(SeqCst)
                } else if state == super::thread::STATUS_BLOCKED_ON_MONITOR_ENTER {
                    r.blocked_on.load(SeqCst)
                } else {
                    0
                } as Oop;
                Sample { thread: jt, state, lock, frames }
            };
            match ids {
                None => threads.iter().map(|&(r, jt)| Some(take(r, jt))).collect(),
                Some(ids) => ids.iter().map(|&id| threads.iter().find(|&&(_, jt)| tid(jt) == id).map(|&(r, jt)| take(r, jt))).collect(),
            }
        });
        if let Some(v) = taken {
            return v;
        }
    }
}

/// A `ThreadInfo` of a sample: its thread, state, lock, stack; no contention statistics (-1).
fn info(t: &mut Thread, s: &Sample) -> JResult<Oop> {
    let ste = crate::loader::boot_class!("java/lang/StackTraceElement").expect("StackTraceElement");
    let stack = rt::alloc_array(t, vm().array_class_of(ste).expect("StackTraceElement[]"), s.frames.len() as i32)?;
    for (i, &(m, bci)) in s.frames.iter().enumerate() {
        let e = crate::boot::construct(t, "java/lang/StackTraceElement", "()V", &[])?;
        super::throwable::fill_element(t, e, m, bci)?;
        // SAFETY: a StackTraceElement[] of frames.len() elements.
        unsafe { obj::store_elem(stack, i, e) };
    }
    // ThreadInfo(thread, state, lockObj, lockOwner, blockedCount, blockedTime, waitedCount,
    // waitedTime, stackTrace); longs take two slots.
    let args = [s.thread as u64, s.state as u64, s.lock as u64, 0, 0, 0, u64::MAX, 0, 0, 0, u64::MAX, 0, stack as u64];
    crate::boot::construct(t, INFO, "(Ljava/lang/Thread;ILjava/lang/Object;Ljava/lang/Thread;JJJJ[Ljava/lang/StackTraceElement;)V", &args)
}

fn infos(t: &mut Thread, samples: &[Option<Sample>]) -> JResult<Oop> {
    let k = crate::loader::load(INFO).expect("ThreadInfo");
    let out = rt::alloc_array(t, vm().array_class_of(k).expect("ThreadInfo[]"), samples.len() as i32)?;
    for (i, s) in samples.iter().enumerate() {
        if let Some(s) = s {
            let o = info(t, s)?;
            // SAFETY: a ThreadInfo[] of samples.len() elements.
            unsafe { obj::store_elem(out, i, o) };
        }
    }
    Ok(out)
}

fn longs(a: Oop) -> Vec<i64> {
    // SAFETY: a live long[].
    unsafe { obj::array_slice::<i64>(a) }.to_vec()
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(C, "getThreads", "()[Ljava/lang/Thread;", super::thread::get_threads);
    // getThreadInfo1(ids, maxDepth, result)
    r.add(C, "getThreadInfo1", "([JI[Ljava/lang/management/ThreadInfo;)V", |t, a| {
        let (ids, depth, result) = (longs(a[0] as Oop), a[1] as i32, a[2] as Oop);
        let samples = sample(t, Some(&ids), depth);
        let filled = infos(t, &samples)?;
        // SAFETY: both arrays have ids.len() elements.
        unsafe {
            for i in 0..ids.len() {
                obj::store_elem(result, i, obj::load_elem(filled, i));
            }
        }
        ret_void()
    });
    // dumpThreads0(ids or null, lockedMonitors, lockedSynchronizers, maxDepth)
    r.add(C, "dumpThreads0", "([JZZI)[Ljava/lang/management/ThreadInfo;", |t, a| {
        let ids = (a[0] != 0).then(|| longs(a[0] as Oop));
        let samples = sample(t, ids.as_deref(), a[3] as i32);
        ret_ref(infos(t, &samples)?)
    });
    // Statistics this VM does not keep: -1, as for a disabled or unsupported measurement.
    for name in ["getThreadTotalCpuTime0", "getThreadUserCpuTime0", "getThreadAllocatedMemory0"] {
        r.add(C, name, "(J)J", |_, _| ret_long(-1));
    }
    for name in ["getThreadTotalCpuTime1", "getThreadUserCpuTime1", "getThreadAllocatedMemory1"] {
        r.add(C, name, "([J[J)V", |_, a| {
            // SAFETY: a long[] result of the ids' length.
            unsafe { obj::array_slice::<i64>(a[1] as Oop).fill(-1) };
            ret_void()
        });
    }
    r.add(C, "getTotalThreadAllocatedMemory", "()J", |_, _| ret_long(-1));
    for name in ["setThreadCpuTimeEnabled0", "setThreadAllocatedMemoryEnabled0", "setThreadContentionMonitoringEnabled0"] {
        r.add(C, name, "(Z)V", |_, _| ret_void());
    }
    // Deadlock detection over monitors is not available: no deadlocked threads are reported.
    for name in ["findMonitorDeadlockedThreads0", "findDeadlockedThreads0"] {
        r.add(C, name, "()[Ljava/lang/Thread;", |_, _| ret_ref(std::ptr::null_mut()));
    }
    r.add(C, "resetPeakThreadCount0", "()V", |_, _| ret_void());
    r.add(C, "resetContentionTimes0", "(J)V", |_, _| ret_void());
}
