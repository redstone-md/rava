//! Opt-in measurements of host work; never used for execution admission.
use crate::diag::Counter;
use std::time::Instant;

crate::diag_counter!(pub cold_calls, "aot", "cold_calls", "Cold link attempts");
crate::diag_counter!(pub cold_ns, "aot", "cold_ns", "Inclusive cold link elapsed nanoseconds");
crate::diag_counter!(pub key_ns, "aot", "key_ns", "Prepared directory validation and cache key elapsed nanoseconds");
crate::diag_counter!(pub read_ns, "aot", "read_ns", "Object file read elapsed nanoseconds");
crate::diag_counter!(pub decode_ns, "aot", "decode_ns", "Object decode elapsed nanoseconds");
crate::diag_counter!(pub bind_ns, "aot", "bind_ns", "Object relocation and constant resolution elapsed nanoseconds");
crate::diag_meter!(pub pack, "aot", "pack", "Sampled argument packing before guest execution");
crate::diag_meter!(pub run, "aot", "run", "Prepared entry envelope excluding metered packing and execution");
crate::diag_meter!(pub execute, "aot", "execute", "Prepared machine code excluding metered callbacks and nested calls");
crate::diag_meter!(pub resolve, "aot", "resolve", "Direct prepared target resolution before entering the shared runner");
crate::diag_histogram!(pub cells, "aot", "cells", "Local and spill cells per executed AOT frame");

/// Exact elapsed time for infrequent operations. Nested scopes are inclusive;
/// sums are not CPU time and must not be added to their enclosing scope.
pub struct Elapsed {
    start: Option<Instant>,
    counter: Counter,
}

impl Elapsed {
    pub fn start(counter: Counter) -> Self {
        Self { start: counter.armed().then(Instant::now), counter }
    }
}

impl Drop for Elapsed {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            self.counter.add(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
        }
    }
}
