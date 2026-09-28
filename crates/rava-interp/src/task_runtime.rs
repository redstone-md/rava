//! Resident host workers for Java executor tasks.
//!
//! This is the task-graph seam, not a claim that arbitrary guest bytecode is
//! GIL-free. Workers wait outside the GIL, acquire it for one recursive guest
//! task, publish completion, then release it again. Queue records remain owned by
//! the runtime until completion so both collectors can treat them as roots.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::detached_task::DetachedTaskPlan;
use crate::parallel::GilVmToken;
use crate::{Value, Vm};

const MAX_IN_FLIGHT_TASKS: usize = 256;

#[derive(Clone, Copy, Eq, PartialEq)]
enum TaskState {
    Queued,
    Running,
}

struct TaskRecord {
    task: Value,
    future: Option<Value>,
    preset: Option<Value>,
    detached: Option<DetachedTaskPlan>,
    state: TaskState,
}

struct ActiveRealThread;

impl ActiveRealThread {
    fn enter() -> Self {
        crate::gil::thread_started();
        Self
    }
}

impl Drop for ActiveRealThread {
    fn drop(&mut self) {
        crate::gil::thread_finished();
    }
}

struct WorkerTaskScope {
    gil_held: bool,
    frames_registered: bool,
}

impl WorkerTaskScope {
    fn enter(vm_token: GilVmToken, thread_object: u32) -> Self {
        let mut scope = Self {
            gil_held: false,
            frames_registered: false,
        };
        crate::gil::acquire();
        scope.gil_held = true;
        crate::frames::register();
        scope.frames_registered = true;
        crate::gil::CURRENT_THREAD.with(|current| current.set(thread_object));
        crate::jit::set_current_vm(vm_token.as_mut_ptr());
        scope
    }
}

impl Drop for WorkerTaskScope {
    fn drop(&mut self) {
        crate::jit::set_current_vm(std::ptr::null_mut());
        crate::gil::CURRENT_THREAD.with(|current| current.set(0));
        if self.frames_registered {
            crate::frames::unregister();
        }
        if self.gil_held {
            crate::gil::release();
        }
    }
}

#[derive(Default)]
struct State {
    next_id: u64,
    queue: VecDeque<u64>,
    tasks: HashMap<u64, TaskRecord>,
    worker_threads: Vec<Value>,
    completion_epoch: u64,
    stop: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    work_ready: Condvar,
    completed: Condvar,
}

/// Per-VM executor runtime. Internals are deliberately private: guest code can
/// submit values, wait for a completion epoch, or expose roots to the collector.
pub(crate) struct TaskRuntime {
    shared: Arc<Shared>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Default for TaskRuntime {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared::default()),
            workers: Mutex::new(Vec::new()),
        }
    }
}

impl TaskRuntime {
    pub(crate) fn is_started(&self) -> bool {
        !self
            .workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    pub(crate) fn start(&self, vm_token: GilVmToken, thread_objects: Vec<u32>) -> bool {
        let mut workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        if !workers.is_empty() {
            return true;
        }
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stop {
                return false;
            }
            state.worker_threads = thread_objects.iter().copied().map(Value::Ref).collect();
        }
        for (index, thread_object) in thread_objects.into_iter().enumerate() {
            let shared = Arc::clone(&self.shared);
            let spawn = std::thread::Builder::new()
                .name(format!("rava-executor-{index}"))
                .stack_size(64 * 1024 * 1024)
                .spawn(move || worker_loop(shared, vm_token, thread_object));
            match spawn {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    eprintln!("[EXECUTOR] failed to start worker {index}: {error}");
                    break;
                }
            }
        }
        !workers.is_empty()
    }

    pub(crate) fn enqueue(
        &self,
        task: Value,
        future: Option<Value>,
        preset: Option<Value>,
        detached: Option<DetachedTaskPlan>,
    ) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stop || state.tasks.len() >= MAX_IN_FLIGHT_TASKS {
            return false;
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.tasks.insert(
            id,
            TaskRecord {
                task,
                future,
                preset,
                detached,
                state: TaskState::Queued,
            },
        );
        state.queue.push_back(id);
        drop(state);
        self.shared.work_ready.notify_one();
        true
    }

    pub(crate) fn tracks_future(&self, future: u32) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tasks
            .values()
            .any(|task| matches!(task.future, Some(Value::Ref(h)) if h == future))
    }

    pub(crate) fn completion_epoch(&self) -> u64 {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .completion_epoch
    }

    pub(crate) fn wait_for_completion_after(&self, epoch: u64) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.completion_epoch == epoch && !state.stop {
            state = self
                .shared
                .completed
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    pub(crate) fn visit_roots(&self, mut visit: impl FnMut(&mut Value)) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        for value in &mut state.worker_threads {
            visit(value);
        }
        for task in state.tasks.values_mut() {
            visit(&mut task.task);
            if let Some(future) = &mut task.future {
                visit(future);
            }
            if let Some(preset) = &mut task.preset {
                visit(preset);
            }
        }
    }

    pub(crate) fn shutdown_and_join(&self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stop = true;
            state.queue.clear();
            state
                .tasks
                .retain(|_, task| task.state == TaskState::Running);
        }
        self.shared.work_ready.notify_all();
        self.shared.completed.notify_all();
        let handles = {
            let mut workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *workers)
        };
        for handle in handles {
            let _ = handle.join();
        }
    }
}

impl Shared {
    fn claim(&self) -> Option<u64> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.stop {
                return None;
            }
            if let Some(id) = state.queue.pop_front() {
                if let Some(task) = state.tasks.get_mut(&id) {
                    task.state = TaskState::Running;
                    return Some(id);
                }
                continue;
            }
            state = self
                .work_ready
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    fn snapshot(&self, id: u64) -> Option<(Value, Option<Value>, Option<Value>)> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let task = state.tasks.get(&id)?;
        Some((task.task.clone(), task.future.clone(), task.preset.clone()))
    }

    fn detached(&self, id: u64) -> Option<DetachedTaskPlan> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tasks
            .get(&id)?
            .detached
            .clone()
    }

    fn finish(&self, id: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.tasks.remove(&id);
        state.completion_epoch = state.completion_epoch.wrapping_add(1);
        drop(state);
        self.completed.notify_all();
    }
}

/// Nanoseconds spent INSIDE guest executor tasks, summed across workers (`RAVA_TASK_TIME`).
///
/// This prices the parallelism ceiling before anyone builds for it. A worker acquires the GIL
/// for the whole guest task (see the module header), so executor tasks are concurrent and never
/// PARALLEL; making them parallel can only speed up the fraction of the boot measured here.
pub(crate) static TASK_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static TASK_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn worker_loop(shared: Arc<Shared>, vm_token: GilVmToken, thread_object: u32) {
    crate::gil::set_worker();
    while let Some(id) = shared.claim() {
        let _active = ActiveRealThread::enter();
        let __t0 = std::time::Instant::now();
        if let Some(plan) = shared.detached(id) {
            run_detached_task(&shared, id, vm_token, thread_object, plan);
        } else {
            let _scope = WorkerTaskScope::enter(vm_token, thread_object);
            run_guest_task(&shared, id, vm_token);
        }
        TASK_NS.fetch_add(
            __t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        TASK_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        shared.finish(id);
    }
}

type GuestOutcome = Result<Result<Option<Value>, crate::VmError>, Box<dyn std::any::Any + Send>>;

fn run_guest_task(shared: &Shared, id: u64, vm_token: GilVmToken) {
    let Some((task, _, _)) = shared.snapshot(id) else {
        eprintln!("[EXECUTOR] running task lost its rooted record");
        return;
    };
    // SAFETY: the caller owns the global GIL for the whole access.
    let vm = unsafe { vm_token.vm_mut() };
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vm.apply(&task, Vec::new())));
    complete_task(shared, id, vm, outcome);
}

fn run_detached_task(
    shared: &Shared,
    id: u64,
    vm_token: GilVmToken,
    thread_object: u32,
    plan: DetachedTaskPlan,
) {
    let invocation = {
        let _scope = WorkerTaskScope::enter(vm_token, thread_object);
        let Some((task, _, _)) = shared.snapshot(id) else {
            eprintln!("[EXECUTOR] detached task lost its rooted record");
            return;
        };
        // SAFETY: this worker owns the GIL while it snapshots primitive fields.
        let vm = unsafe { vm_token.vm_mut() };
        match plan.materialize(vm, &task) {
            Some(invocation) => invocation,
            None => {
                // GAP-A01/A05 probe: proven plan failed to materialize → guest body.
                crate::taskgraph_probe::count_detached_fallback();
                run_guest_task(shared, id, vm_token);
                return;
            }
        }
    };

    let mut invocation = invocation;
    let raw = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| invocation.run()));
    let _scope = WorkerTaskScope::enter(vm_token, thread_object);
    // SAFETY: this worker reacquired the GIL before touching the VM or Future.
    let vm = unsafe { vm_token.vm_mut() };
    let outcome: GuestOutcome = match raw {
        Ok(bits) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            invocation.publish(vm, bits)
        })),
        Err(_) => {
            let Some((task, _, _)) = shared.snapshot(id) else {
                eprintln!("[EXECUTOR] detached fallback lost its rooted record");
                return;
            };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vm.apply(&task, Vec::new())))
        }
    };
    complete_task(shared, id, vm, outcome);
}

fn complete_task(shared: &Shared, id: u64, vm: &mut Vm, outcome: GuestOutcome) {
    // The queue record is the authoritative mutable GC root. Re-snapshot after
    // execution because another worker may have released the GIL and triggered GC.
    if let Some((_, future, preset)) = shared.snapshot(id) {
        let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::reflect::complete_queued_task(vm, future, preset, outcome);
        }));
        if completed.is_err() {
            eprintln!("[EXECUTOR] task completion panicked");
        }
    } else {
        eprintln!("[EXECUTOR] running task lost its rooted record");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_records_are_mutable_gc_roots_until_completion() {
        let runtime = TaskRuntime::default();
        assert!(runtime.enqueue(
            Value::Int(1),
            Some(Value::Ref(7)),
            Some(Value::Int(3)),
            None
        ));
        runtime.visit_roots(|value| match value {
            Value::Int(1) => *value = Value::Int(2),
            Value::Ref(7) => *value = Value::Ref(8),
            Value::Int(3) => *value = Value::Int(4),
            _ => {}
        });
        let id = runtime.shared.claim().expect("queued task");
        let snapshot = runtime.shared.snapshot(id).expect("live record");
        assert!(matches!(snapshot.0, Value::Int(2)));
        assert!(matches!(snapshot.1, Some(Value::Ref(8))));
        assert!(matches!(snapshot.2, Some(Value::Int(4))));
        runtime.shared.finish(id);
        assert!(!runtime.tracks_future(8));
    }

    #[test]
    fn completion_epoch_prevents_lost_wakeup() {
        let runtime = TaskRuntime::default();
        assert!(runtime.enqueue(Value::Int(1), Some(Value::Ref(7)), None, None));
        let id = runtime.shared.claim().expect("queued task");
        let epoch = runtime.completion_epoch();

        runtime.shared.finish(id);
        runtime.wait_for_completion_after(epoch);

        assert!(!runtime.tracks_future(7));
        assert_ne!(runtime.completion_epoch(), epoch);
    }

    #[test]
    fn interleaved_running_records_keep_independent_roots() {
        let runtime = TaskRuntime::default();
        assert!(runtime.enqueue(Value::Int(1), None, None, None));
        assert!(runtime.enqueue(Value::Int(2), None, None, None));
        let first = runtime.shared.claim().expect("first task");
        let second = runtime.shared.claim().expect("second task");

        runtime.visit_roots(|value| match value {
            Value::Int(1) => *value = Value::Int(11),
            Value::Int(2) => *value = Value::Int(22),
            _ => {}
        });
        runtime.shared.finish(first);

        let snapshot = runtime
            .shared
            .snapshot(second)
            .expect("second task remains rooted");
        assert!(matches!(snapshot.0, Value::Int(22)));
        runtime.shared.finish(second);
    }

    #[test]
    fn shutdown_discards_queued_roots_and_rejects_new_work() {
        let runtime = TaskRuntime::default();
        assert!(runtime.enqueue(Value::Int(1), Some(Value::Ref(7)), None, None));

        runtime.shutdown_and_join();

        let mut roots = 0;
        runtime.visit_roots(|_| roots += 1);
        assert_eq!(roots, 0);
        assert!(!runtime.enqueue(Value::Int(2), None, None, None));
    }

    #[test]
    fn in_flight_limit_fails_closed_without_growing_roots() {
        let runtime = TaskRuntime::default();
        for value in 0..MAX_IN_FLIGHT_TASKS {
            assert!(runtime.enqueue(Value::Int(value as i32), None, None, None));
        }
        assert!(!runtime.enqueue(Value::Int(-1), None, None, None));

        let mut roots = 0;
        runtime.visit_roots(|_| roots += 1);
        assert_eq!(roots, MAX_IN_FLIGHT_TASKS);
    }

    #[test]
    fn thrown_task_completes_future_exceptionally() {
        let mut vm = Vm::new(crate::class_loader::ClassLoader::new());
        let future = vm.heap.alloc("java/util/concurrent/CompletableFuture");
        vm.heap.put_field(future, "$done", Value::Int(0));
        let thrown = Value::Ref(77);

        crate::reflect::complete_queued_task(
            &mut vm,
            Some(Value::Ref(future)),
            None,
            Ok(Err(crate::VmError::Throw(thrown.clone()))),
        );

        assert!(matches!(vm.heap.get_field(future, "$done"), Value::Int(1)));
        assert_eq!(
            vm.heap.get_field(future, "$error").display(),
            thrown.display()
        );
    }
}
