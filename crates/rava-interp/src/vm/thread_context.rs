use crate::value::Value;
use std::sync::Mutex;

/// Thread-local execution state. Zero shared mutation — every field is exclusively
/// owned by one OS thread during execution.
pub struct ThreadContext<'a> {
    /// TLAB cursor for this thread's young nursery slice.
    pub heap_view: HeapView<'a>,
    /// Temporary GC-root stack for heap values held only in Rust locals.
    pub gc_protect: Vec<Value>,
    /// Logical Java call stack (class, method+desc) for diagnostics.
    pub call_stack: Vec<(String, String)>,
    /// Total ops executed so far.
    pub op_total: u64,
    pub gc_next: usize,
    pub gc_next_real: usize,
    pub call_depth: u32,
    pub green_threads: Vec<crate::GreenThread>,
}

/// Lifetime-bound view into a thread's nursery TLAB.
///
/// Phase 3 tracer: real allocation window (bump/limit) — bump allocs
/// in `ThreadContext` go through this without touching `&mut Vm`.
/// `nursery_base` is the young base for header tagging / `is_young`.
pub struct HeapView<'a> {
    pub bump: *mut u8,
    pub limit: *mut u8,
    pub nursery_base: *mut u8,
    _marker: std::marker::PhantomData<&'a mut ()>,
}

impl<'a> HeapView<'a> {
    pub fn new(bump: *mut u8, limit: *mut u8, nursery_base: *mut u8) -> Self {
        HeapView { bump, limit, nursery_base, _marker: std::marker::PhantomData }
    }

    pub fn empty() -> Self {
        HeapView { bump: std::ptr::null_mut(), limit: std::ptr::null_mut(), nursery_base: std::ptr::null_mut(), _marker: std::marker::PhantomData }
    }

    /// Back-compat: alias for empty(). Call sites that haven't been
    /// threaded with a real TLAB keep compiling; they'll fallback to
    /// the Vm heap path until wired.
    pub fn current() -> Self {
        Self::empty()
    }

    /// Phase 4 FIX — a COHERENT WINDOW over `JIT_CURSOR`, never a second
    /// allocator. `alloc_raw` bumps the ONE nursery frontier these two words
    /// track (the same words `tlab_try_alloc` and compiled code bump), so
    /// the window's local `bump` field is kept as a DIAGNOSTIC mirror only —
    /// it is written AFTER the authoritative bump succeeds, never read to
    /// decide `fit`. This eliminates the rewind: the whole arena frontier
    /// moves through `JIT_CURSOR[0]` exclusively.
    #[inline(always)]
    pub fn alloc_raw(&mut self, size: usize, align: usize) -> Option<*mut u8> {
        // Delegate to the one nursery frontier; None = not published or would
        // not fit, and the caller falls through (no store in either case, so
        // the outline arm can initialise TLAB / spill / abort / collect).
        let p = crate::nextgen::tlab::cursor_bump_raw(size, align)?;
        // Mirror for diagnostics: limit/base stay as published by the last
        // refresh_from_tlab; bump is the frontier of the last successful view
        // alloc, and sync/refresh reconcile them with `young.top`.
        self.bump = unsafe { p.add(size) };
        Some(p)
    }

    /// Fold the authoritative cursor into `young.top` — NEVER the other way
    /// around. The historic `young_set_top(bump-base)` rewound `top` to the
    /// stale `0` a view held while every arena allocation had already advanced
    /// `JIT_CURSOR[0]`, erasing those objects. The cursor is the frontier.
    pub fn sync_to_tlab(&self) {
        crate::nextgen::tlab::sync_young_from_cursor_only();
    }



    /// Refresh this window from the thread-local TLAB (e.g. at region
    /// entry or after a GC that moved the nursery).
    pub fn refresh_from_tlab(&mut self) {
        let (base, top, cap) = crate::nextgen::tlab::young_extent();
        if base == 0 || cap == 0 {
            *self = Self::empty();
            return;
        }
        self.nursery_base = base as *mut u8;
        self.bump = (base + top) as *mut u8;
        self.limit = (base + cap) as *mut u8;
    }
}
// ── Phase 4: thread-local HeapView bridge ──
//
// Workers that run through `WorkerContext` don't always carry a
// `ThreadContext` value by hand. Keep a per-thread HeapView in TLS
// that mirrors the current TLAB window, so `enter`/`drop` can
// refresh/sync it without threading a ctx reference through every
// caller. Code that does carry a ThreadContext uses its own
// `heap_view` field; this TLS is the fallback for the RAII worker path.
thread_local! {
    static HEAP_VIEW_TLS: std::cell::RefCell<HeapView<'static>> = std::cell::RefCell::new(HeapView::empty());
}

/// Refresh the thread-local HeapView from the current TLAB (young_extent).
/// Called at worker entry after `worker_tlab_view` installs the slice.
pub fn refresh_heap_view_from_tlab() {
    HEAP_VIEW_TLS.with(|c| c.borrow_mut().refresh_from_tlab());
}

/// Sync the thread-local HeapView back to the TLAB cursor.
/// Called at worker exit before `worker_tlab_clear`.
pub fn sync_heap_view_to_tlab() {
    HEAP_VIEW_TLS.with(|c| c.borrow().sync_to_tlab());
}

/// Try a bump alloc through the thread-local HeapView. Returns None if
/// the window is empty/exhausted.
pub fn tls_alloc_raw(size: usize, align: usize) -> Option<*mut u8> {
    HEAP_VIEW_TLS.with(|c| c.borrow_mut().alloc_raw(size, align))
}

/// Access the thread-local HeapView mutably.
pub fn with_heap_view<R>(f: impl FnOnce(&mut HeapView<'static>) -> R) -> R {
    HEAP_VIEW_TLS.with(|c| f(&mut c.borrow_mut()))
}

thread_local! {
    /// Worker-owned execution context. Main-thread calls keep using the Vm fallback
    /// until the general execution path is threaded with an explicit context.
    static ACTIVE_THREAD_CONTEXT: std::cell::RefCell<Option<Box<ThreadContext<'static>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Stable addresses of active worker-owned temporary-root vectors. The owning
/// contexts are boxed and remain registered until their guards drop; collectors
/// may dereference these only after all mutators are stopped.
static GC_PROTECT_ROOTS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn register_gc_protect_roots(address: usize) {
    GC_PROTECT_ROOTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(address);
}

fn unregister_gc_protect_roots(address: usize) {
    GC_PROTECT_ROOTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|registered| *registered != address);
}

/// Snapshot worker root-vector addresses for registration tests.
#[cfg(test)]
pub(crate) fn gc_protect_roots() -> Vec<usize> {
    GC_PROTECT_ROOTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// Visit registered worker roots while every owning worker is stopped.
///
/// # Safety
/// Callers must guarantee that each registered context is alive and its owner
/// cannot access or mutate the root vector until this function returns. The
/// registry mutex pins registrations against guard teardown; it does not stop
/// worker execution.
pub(crate) unsafe fn with_registered_gc_protect_roots_mut(
    only: Option<&[usize]>,
    mut visit: impl FnMut(&mut Vec<Value>),
) {
    let roots = GC_PROTECT_ROOTS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut visit_address = |address: usize| {
        // SAFETY: guaranteed by the caller; the registry lock keeps the boxed
        // context registered until this visit finishes.
        visit(unsafe { &mut *(address as *mut Vec<Value>) });
    };
    if let Some(only) = only {
        for &address in only {
            assert!(roots.contains(&address), "worker root is not registered");
            visit_address(address);
        }
    } else {
        for &address in roots.iter() {
            visit_address(address);
        }
    }
}

/// Installs a fresh worker context and restores any outer context on drop.
pub(crate) struct WorkerThreadContextGuard {
    previous: Option<Box<ThreadContext<'static>>>,
    roots_address: usize,
}

impl WorkerThreadContextGuard {
    pub(crate) fn install() -> Self {
        let mut context = Box::new(ThreadContext::new(0, 0));
        let roots_address = std::ptr::addr_of_mut!(context.gc_protect) as usize;
        register_gc_protect_roots(roots_address);
        let previous = ACTIVE_THREAD_CONTEXT.with(|slot| slot.borrow_mut().replace(context));
        Self {
            previous,
            roots_address,
        }
    }
}

impl Drop for WorkerThreadContextGuard {
    fn drop(&mut self) {
        unregister_gc_protect_roots(self.roots_address);
        let previous = self.previous.take();
        ACTIVE_THREAD_CONTEXT.with(|slot| *slot.borrow_mut() = previous);
    }
}


/// Push/pop the logical call trace into the worker context when installed, otherwise
/// retain the coordinator's existing Vm-owned stack.
pub(crate) fn push_call(
    fallback: &mut Vec<(String, String)>,
    class: String,
    method: String,
) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        let mut active = slot.borrow_mut();
        if let Some(context) = active.as_mut() {
            context.push_call(class, method);
        } else {
            fallback.push((class, method));
        }
    });
}

pub(crate) fn pop_call(fallback: &mut Vec<(String, String)>) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        let mut active = slot.borrow_mut();
        if let Some(context) = active.as_mut() {
            context.pop_call();
        } else {
            fallback.pop();
        }
    });
}

pub(crate) fn with_call_stack<R>(
    fallback: &[(String, String)],
    read: impl FnOnce(&[(String, String)]) -> R,
) -> R {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        let active = slot.borrow();
        read(active.as_ref().map_or(fallback, |context| &context.call_stack))
    })
}

/// Account guest operations on the current worker without touching coordinator state.
pub(crate) fn add_op_total(count: u64) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.op_total = context.op_total.saturating_add(count);
        }
    });
}

/// Read the active thread's GC pressure marks, or the coordinator's Vm fallback.
pub(crate) fn gc_thresholds(fallback: &crate::vmstate::Vm) -> (usize, usize) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        slot.borrow().as_ref().map_or(
            (fallback.gc_next, fallback.gc_next_real),
            |context| (context.gc_next, context.gc_next_real),
        )
    })
}

/// Update the active thread's GC pressure marks without exposing TLS-owned references.
pub(crate) fn set_gc_thresholds(
    fallback: &mut crate::vmstate::Vm,
    gc_next: usize,
    gc_next_real: usize,
) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        let mut active = slot.borrow_mut();
        if let Some(context) = active.as_mut() {
            context.gc_next = gc_next;
            context.gc_next_real = gc_next_real;
        } else {
            fallback.gc_next = gc_next;
            fallback.gc_next_real = gc_next_real;
        }
    });
}


// ── VM root-stack facade (context-or-fallback) ──
//
// Every `Vm.gc_protect` producer/consumer routes through these helpers. When a
// `WorkerThreadContextGuard` is active on the current thread the operation lands in
// the worker-owned `ThreadContext.gc_protect` (registered with the collector through
// `GC_PROTECT_ROOTS`); otherwise it lands in the passed Vm-owned fallback vector,
// exactly as before. No helper returns a reference or iterator into the TLS-owned
// vector: reads are cloned, iteration is a synchronous closure, and no closure may
// run guest code (which could re-enter these helpers) while a TLS borrow is live.

/// Push a temporary root for the caller's scope.
#[inline]
pub(crate) fn push_root(fallback: &mut Vec<Value>, v: Value) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.gc_protect.push(v);
        } else {
            fallback.push(v);
        }
    });
}

/// Pop the most recently pushed root.
#[inline]
pub(crate) fn pop_root(fallback: &mut Vec<Value>) -> Option<Value> {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.gc_protect.pop()
        } else {
            fallback.pop()
        }
    })
}

/// Current root-stack depth (the `gc_protect.len()` shape).
#[inline]
pub(crate) fn root_len(fallback: &[Value]) -> usize {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(fallback.len(), |context| context.gc_protect.len())
    })
}

/// Truncate the root stack back to a saved depth.
#[inline]
pub(crate) fn truncate_roots(fallback: &mut Vec<Value>, len: usize) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.gc_protect.truncate(len);
        } else {
            fallback.truncate(len);
        }
    });
}

/// Pre-allocate room for `additional` more roots.
#[inline]
pub(crate) fn reserve_roots(fallback: &mut Vec<Value>, additional: usize) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.gc_protect.reserve(additional);
        } else {
            fallback.reserve(additional);
        }
    });
}

/// Push every value of `iter` as a root (the `gc_protect.extend` shape).
#[inline]
pub(crate) fn extend_roots<I: IntoIterator<Item = Value>>(fallback: &mut Vec<Value>, iter: I) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.gc_protect.extend(iter);
        } else {
            fallback.extend(iter);
        }
    });
}

/// Move all of `other` onto the root stack (the `gc_protect.append` shape).
#[inline]
pub(crate) fn append_roots(fallback: &mut Vec<Value>, other: &mut Vec<Value>) {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.gc_protect.append(other);
        } else {
            fallback.append(other);
        }
    });
}

/// Split the root stack at `at` and take the tail (the `gc_protect.split_off` shape).
#[inline]
pub(crate) fn split_off_roots(fallback: &mut Vec<Value>, at: usize) -> Vec<Value> {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.gc_protect.split_off(at)
        } else {
            fallback.split_off(at)
        }
    })
}

/// Cloned read of one root slot; `None` past the end (the `gc_protect.get` shape).
#[inline]
pub(crate) fn root_get(fallback: &[Value], index: usize) -> Option<Value> {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        let active = slot.borrow();
        let roots = active.as_ref().map_or(fallback, |context| context.gc_protect.as_slice());
        roots.get(index).cloned()
    })
}

/// Cloned read of one root slot; panics out of bounds exactly like `Index`.
#[inline]
pub(crate) fn root_index(fallback: &[Value], index: usize) -> Value {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        let active = slot.borrow();
        let roots = active.as_ref().map_or(fallback, |context| context.gc_protect.as_slice());
        roots[index].clone()
    })
}

/// In-place slot replacement; panics out of bounds exactly like `Index` assignment.
#[allow(dead_code)] // contract-mandated seam; today only in-module tests exercise it
#[inline]
pub(crate) fn root_replace(fallback: &mut Vec<Value>, index: usize, value: Value) -> Value {
    ACTIVE_THREAD_CONTEXT.with(|slot| {
        let mut active = slot.borrow_mut();
        let roots = active
            .as_mut()
            .map(|context| &mut context.gc_protect)
            .unwrap_or(fallback);
        std::mem::replace(&mut roots[index], value)
    })
}


/// Truncate the active root stack back to a saved depth on scope exit. The guard
/// retains the Vm fallback pointer and the depth established by the push. It is
/// unsafe because the fallback and active context must remain valid for the scope.
pub(crate) struct RootTrunc {
    fallback: *mut Vec<Value>,
    base: usize,
}

impl RootTrunc {
    /// # Safety
    /// `fallback` must remain alive until the returned guard is dropped, and
    /// the active thread context (if any) must remain the same for the scope.
    #[inline]
    pub(crate) unsafe fn new(fallback: &mut Vec<Value>, base: usize) -> Self {
        debug_assert!(
            root_len(fallback) >= base,
            "root scope base is above the current stack depth"
        );
        Self {
            fallback,
            base,
        }
    }
}

impl Drop for RootTrunc {
    fn drop(&mut self) {
        // SAFETY: upheld by the constructor contract; the facade chooses the
        // same active-context-or-fallback stack that received the scope's pushes.
        unsafe { truncate_roots(&mut *self.fallback, self.base) };
    }
}


pub(crate) fn render_call_stack(call_stack: &[(String, String)]) -> String {
    call_stack
        .iter()
        .rev()
        .map(|(class, method)| format!("  at {class}.{method}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_call_stack_is_scoped_thread_local_and_formatted_identically() {
        let mut fallback = vec![("Serial".to_owned(), "main()V".to_owned())];
        assert_eq!(
            with_call_stack(&fallback, render_call_stack),
            "  at Serial.main()V"
        );

        let worker = WorkerThreadContextGuard::install();
        push_call(&mut fallback, "Worker".to_owned(), "run()V".to_owned());
        push_call(&mut fallback, "Nested".to_owned(), "call()V".to_owned());
        assert_eq!(
            with_call_stack(&fallback, render_call_stack),
            "  at Nested.call()V\n  at Worker.run()V"
        );

        let other_thread = std::thread::spawn(|| {
            with_call_stack(&[], |stack| stack.is_empty())
        })
        .join()
        .expect("thread-local call stack probe should join");
        assert!(other_thread);

        pop_call(&mut fallback);
        pop_call(&mut fallback);
        assert!(with_call_stack(&fallback, |stack| stack.is_empty()));
        drop(worker);
        assert_eq!(
            with_call_stack(&fallback, render_call_stack),
            "  at Serial.main()V"
        );
    }

    #[test]
    fn worker_gc_root_vectors_remain_registered_across_nested_contexts() {
        fn active_roots_address() -> usize {
            ACTIVE_THREAD_CONTEXT.with(|slot| {
                let active = slot.borrow();
                std::ptr::addr_of!(active.as_ref().expect("context installed").gc_protect)
                    as usize
            })
        }

        let outer = WorkerThreadContextGuard::install();
        let outer_roots = active_roots_address();
        assert!(gc_protect_roots().contains(&outer_roots));

        let inner = WorkerThreadContextGuard::install();
        let inner_roots = active_roots_address();
        assert_ne!(outer_roots, inner_roots);
        let registered = gc_protect_roots();
        assert!(registered.contains(&outer_roots));
        assert!(registered.contains(&inner_roots));

        drop(inner);
        let registered = gc_protect_roots();
        assert!(registered.contains(&outer_roots));
        assert!(!registered.contains(&inner_roots));

        drop(outer);
        assert!(!gc_protect_roots().contains(&outer_roots));
    }

    #[test]
    fn registered_gc_roots_remain_live_while_the_owner_is_parked() {
        let (root_tx, root_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            let _context = WorkerThreadContextGuard::install();
            let address = ACTIVE_THREAD_CONTEXT.with(|slot| {
                let mut active = slot.borrow_mut();
                let context = active.as_mut().expect("context installed");
                context.gc_protect.push(Value::Int(42));
                std::ptr::addr_of!(context.gc_protect) as usize
            });
            root_tx.send(address).unwrap();
            release_rx.recv().unwrap();
        });

        let address = root_rx.recv().unwrap();
        assert!(gc_protect_roots().contains(&address));
        // SAFETY: the owner is blocked on `release_rx` and its guard keeps the
        // boxed context registered and alive until after this read.
        let roots = unsafe { &*(address as *const Vec<Value>) };
        assert!(matches!(roots.as_slice(), [Value::Int(42)]));

        release_tx.send(()).unwrap();
        owner.join().unwrap();
        assert!(!gc_protect_roots().contains(&address));
    }

    #[test]
    fn registered_worker_roots_can_be_scanned_only_while_owner_is_parked() {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            let _context = WorkerThreadContextGuard::install();
            let address = ACTIVE_THREAD_CONTEXT.with(|slot| {
                let mut active = slot.borrow_mut();
                let context = active.as_mut().expect("context installed");
                context.gc_protect.push(Value::Int(42));
                std::ptr::addr_of_mut!(context.gc_protect) as usize
            });
            ready_tx.send(address).unwrap();
            resume_rx.recv().unwrap();
            ACTIVE_THREAD_CONTEXT.with(|slot| {
                assert!(matches!(
                    slot.borrow()
                        .as_ref()
                        .expect("context remains installed")
                        .gc_protect
                        .as_slice(),
                    [Value::Int(43)]
                ));
            });
        });

        let address = ready_rx.recv().unwrap();
        // SAFETY: the owner is blocked on resume_rx until after this root scan.
        unsafe {
            with_registered_gc_protect_roots_mut(Some(&[address]), |roots| {
                assert!(matches!(roots.as_slice(), [Value::Int(42)]));
                roots[0] = Value::Int(43);
            });
        }
        resume_tx.send(()).unwrap();
        owner.join().unwrap();
    }

    #[test]
    fn worker_operation_count_is_context_local() {
        let _context = WorkerThreadContextGuard::install();
        add_op_total(17);
        let total = ACTIVE_THREAD_CONTEXT.with(|slot| {
            slot.borrow()
                .as_ref()
                .expect("context installed")
                .op_total
        });
        assert_eq!(total, 17);
    }


    #[test]
    fn gc_thresholds_are_thread_local_with_vm_fallback() {
        let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
        vm.gc_next = 17;
        vm.gc_next_real = 23;

        assert_eq!(gc_thresholds(&vm), (17, 23));
        set_gc_thresholds(&mut vm, 31, 47);
        assert_eq!((vm.gc_next, vm.gc_next_real), (31, 47));

        let worker = WorkerThreadContextGuard::install();
        assert_eq!(gc_thresholds(&vm), (usize::MAX, usize::MAX));
        assert_eq!(vm.maybe_gc(), 0);
        assert_eq!(gc_thresholds(&vm), (usize::MAX, usize::MAX));
        set_gc_thresholds(&mut vm, 3, 5);
        assert_eq!(gc_thresholds(&vm), (3, 5));
        assert_eq!((vm.gc_next, vm.gc_next_real), (31, 47));
        drop(worker);

        assert_eq!(gc_thresholds(&vm), (31, 47));
    }

    #[test]
    fn root_facade_routes_to_active_context_and_falls_back_to_vm() {
        let mut fallback: Vec<Value> = Vec::new();
        push_root(&mut fallback, Value::Int(1));
        assert_eq!(root_len(&fallback), 1);
        assert_eq!(root_index(&fallback, 0).as_int(), Some(1));

        let _worker = WorkerThreadContextGuard::install();
        push_root(&mut fallback, Value::Int(2));
        assert_eq!(root_len(&fallback), 1); // the Vm fallback is untouched
        assert_eq!(root_index(&fallback, 0).as_int(), Some(2));
        reserve_roots(&mut fallback, 4);
        extend_roots(&mut fallback, [Value::Int(3), Value::Int(4)]);
        {
            let mut tail = vec![Value::Int(5)];
            append_roots(&mut fallback, &mut tail);
            assert!(tail.is_empty());
        }
        assert_eq!(root_get(&fallback, 0).as_ref().map(|v| v.as_int()), Some(Some(2)));
        assert_eq!(root_replace(&mut fallback, 1, Value::Int(31)).as_int(), Some(3));
        assert_eq!(
            split_off_roots(&mut fallback, 2).iter().map(|v| v.as_int().unwrap()).collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(root_len(&fallback), 2);
        assert_eq!(pop_root(&mut fallback).and_then(|v| v.as_int()), Some(31));
        assert_eq!(root_len(&fallback), 1);
        drop(_worker);
        // The worker context's roots died with it; the fallback still holds the
        // pre-install root.
        assert_eq!(root_len(&fallback), 1);

        push_root(&mut fallback, Value::Int(7));
        assert_eq!(root_len(&fallback), 2);
        truncate_roots(&mut fallback, 0);
        assert_eq!(root_len(&fallback), 0);
    }

    #[test]
    fn root_trunc_guard_truncates_the_active_context_on_drop() {
        let mut fallback: Vec<Value> = Vec::new();
        let _worker = WorkerThreadContextGuard::install();
        push_root(&mut fallback, Value::Int(1));
        let base = root_len(&fallback);
        push_root(&mut fallback, Value::Int(2));
        push_root(&mut fallback, Value::Int(3));
        {
            // SAFETY: fallback and the active context outlive this lexical scope.
            let _scope = unsafe { RootTrunc::new(&mut fallback, base) };
            assert_eq!(root_len(&fallback), 3);
        }
        assert_eq!(root_len(&fallback), base);
        drop(_worker);
        assert_eq!(root_len(&fallback), 0);
    }

    #[test]
    fn root_trunc_guard_truncates_the_vm_fallback_on_drop() {
        let mut fallback = vec![Value::Int(1)];
        {
            // SAFETY: fallback outlives the guard and no context is installed.
            let _scope = unsafe { RootTrunc::new(&mut fallback, 1) };
            fallback.push(Value::Int(2));
        }
        assert_eq!(fallback.len(), 1);
        assert!(matches!(fallback[0], Value::Int(1)));
    }
}

impl<'a> ThreadContext<'a> {
    pub fn new(mem_limit: usize, _max_depth: u32) -> Self {
        let gc_next = if mem_limit > 0 { mem_limit / 4 } else { usize::MAX };
        let gc_next_real = if mem_limit > 0 { mem_limit / 4 } else { usize::MAX };
        ThreadContext {
            heap_view: HeapView::empty(),
            gc_protect: Vec::new(),
            call_stack: Vec::new(),
            op_total: 0,
            gc_next,
            gc_next_real,
            call_depth: 0,
            green_threads: Vec::new(),
        }
    }
    /// Push a Java frame onto the diagnostic call stack. Replaces `vm.call_stack.push`.
    #[inline]
    pub fn push_call(&mut self, class: String, method: String) {
        self.call_stack.push((class, method));
        self.call_depth += 1;
    }
    #[inline]
    pub fn pop_call(&mut self) {
        self.call_stack.pop();
        self.call_depth = self.call_depth.saturating_sub(1);
    }
    /// Temporarily protect a heap value across an allocation-heavy call. Replaces `vm.gc_protect.push`.
    #[inline]
    pub fn protect(&mut self, v: Value) {
        self.gc_protect.push(v);
    }
    #[inline]
    pub fn unprotect(&mut self) -> Option<Value> {
        self.gc_protect.pop()
    }
}

/// Thin alias for the shared part of `Vm` at Phase 1.
/// `SharedVm == Vm`; every `&Vm` site is a valid `&SharedVm` site.
pub type SharedVm = crate::vmstate::Vm;
