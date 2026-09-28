//! Opt-in compiler-boundary diagnostics. Never consulted for admission.
use std::sync::{atomic::{AtomicU64, Ordering}, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::cell::RefCell;
use std::fmt::Write;

/// One driver invocation. IDs distinguish nested and concurrent compilations;
/// elapsed times include waits and must not be summed as CPU time.
pub(crate) struct CompileTrace(Option<TraceState>);

struct TraceState {
    id: u64,
    bytes: usize,
    start: Instant,
    epoch_us: u128,
    phases: RefCell<Vec<Phase>>,
}

struct Phase {
    name: &'static str,
    values: usize,
    blocks: usize,
    elapsed_us: u128,
}

impl CompileTrace {
    pub(crate) fn new(bytes: usize) -> Self {
        static ON: OnceLock<bool> = OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("RAVA_TRACE_COMPILE_PHASES").is_some()) {
            return Self(None);
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(Some(TraceState {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            bytes,
            epoch_us: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros(),
            start: Instant::now(),
            phases: RefCell::new(Vec::with_capacity(4)),
        }))
    }

    pub(crate) fn note(&self, function: &cranelift::codegen::ir::Function, phase: &'static str) {
        if let Some(s) = &self.0 {
            let elapsed_us = s.start.elapsed().as_micros();
            s.phases.borrow_mut().push(Phase {
                name: phase,
                values: function.dfg.num_values(),
                blocks: function.layout.blocks().count(),
                elapsed_us,
            });
        }
    }
}

impl Drop for CompileTrace {
    fn drop(&mut self) {
        if let Some(s) = &self.0 {
            // Capture the end BEFORE formatting or taking stderr's global lock.
            // Concurrent compiler threads must not charge diagnostic I/O to a phase.
            let elapsed_us = s.start.elapsed().as_micros();
            let mut output = String::new();
            for phase in s.phases.borrow().iter() {
                let _ = writeln!(output, "[COMPILE-PHASE] bytecode={} phase={} values={} blocks={} id={} elapsed_us={}",
                    s.bytes, phase.name, phase.values, phase.blocks, s.id, phase.elapsed_us);
            }
            let _ = writeln!(output, "[COMPILE-END] id={} bytecode={} start_epoch_us={} elapsed_us={} thread={:?}",
                s.id, s.bytes, s.epoch_us, elapsed_us, std::thread::current().id());
            eprint!("{output}");
        }
    }
}
