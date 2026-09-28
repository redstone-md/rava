//! How the interpreter loop ends, and the per-thread pc bound that can end it early.

use crate::opcodes::*;

/// How `run` exited the current frame. `Normal` = the method returned (with its
/// value). `Call` = a Java-method invoke was reached in trampoline mode: the
/// pending call is staged in `vm.pending` and the driver (`Vm::execute_stackful`)
/// must push a callee frame. In the default RECURSIVE mode `vm.trampoline` is
/// false, no invoke ever stages a pending call, and `run` only ever returns
/// `Normal` — identical to the old `Result<Option<Value>>` behaviour.
pub(crate) enum RunExit {
    Normal(Option<Value>),
    Call,
    /// (next-gen-gc, M3 T2) A bounded run reached the `stop_pc` set via `run_stop_set` — used by
    /// the counted-loop fork to execute just the loop BODY `[body_start, body_end)` per index on a
    /// worker, then return with `frame.pc == stop_pc` (no return value; stack left as the body
    /// left it, which for a statement body is empty).
    #[cfg(feature = "next-gen-gc")]
    Bounded,
    /// A blocking call (`Thread.sleep`/`Object.wait`) was reached at a frame
    /// boundary in trampoline mode: pc is already advanced past it, the request
    /// is staged in `vm.yield_request`, and the scheduler parks this green thread
    /// and resumes another. On resume the frame continues past the blocking call.
    Yield,
    /// (next-gen-gc) A `new` found the arena nursery full. `run` exits WITHOUT
    /// advancing PC, so the driver (`drive_stack`/`drive_green`) — where no frame
    /// is borrowed — runs a safepoint Cheney collection (`arena_safepoint_gc`),
    /// then re-runs the frame, re-executing the same `new` (now satisfiable).
    #[cfg(feature = "next-gen-gc")]
    NurseryFull,
    /// (next-gen-gc) A `new` of an un-initialised class in trampoline mode. `run`
    /// exits with PC UNCHANGED; the driver pushes the class's `<clinit>` frame(s)
    /// (super-first) onto the explicit stack + drives them — so a clinit's OWN
    /// allocations run trampolined and can safepoint — then re-runs the `new`.
    #[cfg(feature = "next-gen-gc")]
    ClinitPending(std::sync::Arc<crate::class_loader::LoadedClass>),
}

/// M3 T2 — a per-thread PC bound for `run`: while set (< usize::MAX), `run` returns
/// `RunExit::Bounded` as soon as `frame.pc` reaches it, instead of running to the method's return.
/// The counted-loop fork sets it to the loop body's end so a worker executes exactly the body per
/// index. Read once at `run` entry (near-zero per-op cost); default usize::MAX = unbounded.
#[cfg(feature = "next-gen-gc")]
thread_local! {
    pub(crate) static RUN_STOP_PC: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}
