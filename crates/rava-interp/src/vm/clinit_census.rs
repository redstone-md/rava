//! Opt-in CPU attribution for first-time class initialization.
//!
//! Compile with `clinit-census` and set `RAVA_CLINIT_CENSUS=1`. This module only
//! records and reports; it must never influence class loading or execution.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const REPORT_INTERVAL: Duration = Duration::from_secs(1);
const TOP_CLASSES: usize = 12;

thread_local! {
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

struct Frame {
    class_id: usize,
    class_name: String,
    started_ns: u64,
    nested_ns: u64,
}

#[derive(Default)]
struct ClassTotals {
    name: String,
    activations: u64,
    inclusive_ns: u64,
    excluding_nested_ns: u64,
}

#[derive(Default)]
struct Census {
    by_class: HashMap<usize, ClassTotals>,
    root_activations: u64,
    root_cpu_ns: u64,
    clock_failures: u64,
}

struct Report {
    root_activations: u64,
    root_cpu_ns: u64,
    clock_failures: u64,
    top_classes: Vec<(String, u64, u64, u64)>,
}

/// RAII scope around a first-time `ensure_init`; always unwinds the nesting stack.
pub(crate) struct Scope;

impl Scope {
    pub(crate) fn begin(class: &crate::class_loader::LoadedClass) -> Option<Self> {
        if !enabled() {
            return None;
        }
        let Some(started_ns) = thread_cpu_ns() else {
            warn_clock_unavailable();
            return None;
        };
        STACK.with(|stack| {
            stack.borrow_mut().push(Frame {
                class_id: class as *const _ as usize,
                class_name: class.file.this_class.to_string(),
                started_ns,
                nested_ns: 0,
            });
        });
        Some(Self)
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let ended_ns = thread_cpu_ns();
        let completed = STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            let frame = stack.pop()?;
            let elapsed_ns = ended_ns
                .map(|end| end.saturating_sub(frame.started_ns))
                .unwrap_or(0);
            let is_root = if let Some(parent) = stack.last_mut() {
                parent.nested_ns = parent.nested_ns.saturating_add(elapsed_ns);
                false
            } else {
                true
            };
            Some((frame, elapsed_ns, is_root))
        });
        let Some((frame, inclusive_ns, is_root)) = completed else {
            return;
        };

        let mut census = census()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if ended_ns.is_none() {
            census.clock_failures += 1;
        }
        let class = census.by_class.entry(frame.class_id).or_default();
        class.name = frame.class_name;
        class.activations += 1;
        class.inclusive_ns = class.inclusive_ns.saturating_add(inclusive_ns);
        class.excluding_nested_ns = class
            .excluding_nested_ns
            .saturating_add(exclusive_ns(inclusive_ns, frame.nested_ns));
        if is_root {
            census.root_activations += 1;
            census.root_cpu_ns = census.root_cpu_ns.saturating_add(inclusive_ns);
        }
    }
}

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| std::env::var_os("RAVA_CLINIT_CENSUS").is_some());
    if enabled {
        start_reporter();
    }
    enabled
}

fn start_reporter() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let result = std::thread::Builder::new()
            .name("rava-clinit-census".into())
            .spawn(report_loop);
        if let Err(error) = result {
            eprintln!("[CLINIT-CENSUS] reporter unavailable: {error}");
        }
    });
}

fn report_loop() {
    let mut last_reported_roots = 0;
    loop {
        std::thread::sleep(REPORT_INTERVAL);
        let report = {
            let census = census()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if census.root_activations == last_reported_roots {
                None
            } else {
                last_reported_roots = census.root_activations;
                Some(make_report(&census))
            }
        };
        if let Some(report) = report {
            emit_report(report);
        }
    }
}

fn census() -> &'static Mutex<Census> {
    static CENSUS: OnceLock<Mutex<Census>> = OnceLock::new();
    CENSUS.get_or_init(|| Mutex::new(Census::default()))
}

fn exclusive_ns(inclusive_ns: u64, nested_ns: u64) -> u64 {
    inclusive_ns.saturating_sub(nested_ns)
}

fn make_report(census: &Census) -> Report {
    let mut classes = census
        .by_class
        .values()
        .map(|class| {
            (
                class.name.clone(),
                class.activations,
                class.inclusive_ns,
                class.excluding_nested_ns,
            )
        })
        .collect::<Vec<_>>();
    classes.sort_unstable_by(|left, right| right.2.cmp(&left.2));
    classes.truncate(TOP_CLASSES);
    Report {
        root_activations: census.root_activations,
        root_cpu_ns: census.root_cpu_ns,
        clock_failures: census.clock_failures,
        top_classes: classes,
    }
}

fn emit_report(report: Report) {
    eprintln!(
        "[CLINIT-CENSUS] roots={} root_cpu_ms={} clock_failures={} top_classes={}",
        report.root_activations,
        report.root_cpu_ns / 1_000_000,
        report.clock_failures,
        report.top_classes.len()
    );
    for (name, activations, inclusive_ns, excluding_nested_ns) in report.top_classes {
        eprintln!(
            "[CLINIT-CENSUS] class={} activations={} inclusive_ms={} excluding_nested_ms={}",
            name,
            activations,
            inclusive_ns / 1_000_000,
            excluding_nested_ns / 1_000_000
        );
    }
}

#[cfg(target_os = "linux")]
fn thread_cpu_ns() -> Option<u64> {
    use std::os::raw::{c_int, c_long};

    #[repr(C)]
    struct Timespec {
        seconds: c_long,
        nanoseconds: c_long,
    }

    unsafe extern "C" {
        fn clock_gettime(clock_id: c_int, time: *mut Timespec) -> c_int;
    }

    const CLOCK_THREAD_CPUTIME_ID: c_int = 3;
    let mut time = Timespec {
        seconds: 0,
        nanoseconds: 0,
    };
    if unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut time) } != 0 {
        return None;
    }
    u64::try_from(time.seconds)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(time.nanoseconds).ok()?)
}

#[cfg(not(target_os = "linux"))]
fn thread_cpu_ns() -> Option<u64> {
    None
}

fn warn_clock_unavailable() {
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!("[CLINIT-CENSUS] unavailable: Linux thread CPU clock required");
    });
}

#[cfg(test)]
mod tests {
    use super::{exclusive_ns, thread_cpu_ns};

    #[test]
    fn nested_initializer_cpu_is_excluded_from_parent_self_time() {
        assert_eq!(exclusive_ns(100, 30), 70);
        assert_eq!(exclusive_ns(30, 100), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_thread_cpu_clock_is_available_and_monotonic() {
        let before = thread_cpu_ns().expect("thread CPU clock");
        let after = thread_cpu_ns().expect("thread CPU clock");
        assert!(after >= before);
    }
}
