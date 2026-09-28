//! Diagnostic validation of moving-GC root forwarding.
//!
//! This runs only with `RAVA_PROBE_STALE`; it does not affect the root set or
//! collector decisions. It independently searches each registered root source
//! for pointers the just-completed collection moved but failed to rewrite.

use crate::{Frame, Value, Vm};
use std::sync::Arc;

/// Report stale pointers remaining in any mutable Value or cell root after writeback.
pub(super) fn validate_writeback(
    vm: &mut Vm,
    heap_value_locs: &[*mut Value],
    heap_cell_locs: &[*mut i64],
    cell_locs: &[*mut i64],
) {
    if !crate::nextgen::fwd_log_enabled() {
        return;
    }

    let check_value = |value: &Value, source: &str| {
        if let Value::Obj(pointer) = value {
            if let Some(forwarded) = crate::nextgen::fwd_this_lookup(pointer.addr()) {
                eprintln!(
                    "[WRITEBACK-MISS-ROOT] {source} holds {:#x} (forwarded to {forwarded:#x} this GC) — NOT gathered",
                    pointer.addr()
                );
            }
        }
    };
    let check_cell = |cell: i64, source: &str| {
        let raw = cell as u64;
        if raw != 0 && raw & 7 == 0 {
            if let Some(forwarded) = crate::nextgen::fwd_this_lookup(raw) {
                eprintln!(
                    "[WRITEBACK-MISS-CELL] {source} holds {raw:#x} (forwarded to {forwarded:#x} this GC) — NOT gathered"
                );
            }
        }
    };

    for &frame_ptr in &crate::frames::all() {
        let frame = unsafe { &*(frame_ptr as *const Frame) };
        let class = &frame.class.file.this_class;
        for (index, value) in frame.locals().iter().enumerate() {
            check_value(value, &format!("frames::all {class} local[{index}]"));
        }
        for (index, value) in frame.stack().iter().enumerate() {
            check_value(value, &format!("frames::all {class} stack[{index}]"));
        }
    }

    for frame_ptr in crate::frames::exec_all() {
        let frames = unsafe { &*(frame_ptr as *const Vec<(Frame, Arc<str>, Arc<str>)>) };
        for (frame, _, _) in frames {
            let class = &frame.class.file.this_class;
            for (index, value) in frame.locals().iter().enumerate() {
                check_value(value, &format!("exec_frames {class} local[{index}]"));
            }
            for (index, value) in frame.stack().iter().enumerate() {
                check_value(value, &format!("exec_frames {class} stack[{index}]"));
            }
        }
    }

    for thread in &vm.green_threads {
        for (frame, _, _) in &thread.frames {
            let class = &frame.class.file.this_class;
            for (index, value) in frame.locals().iter().enumerate() {
                check_value(value, &format!("green_threads {class} local[{index}]"));
            }
            for (index, value) in frame.stack().iter().enumerate() {
                check_value(value, &format!("green_threads {class} stack[{index}]"));
            }
        }
    }

    crate::execution_state::visit_pending_roots(|value| check_value(value, "pending exception"));
    vm.task_runtime
        .visit_roots(|value| check_value(value, "task runtime"));
    vm.lane
        .visit_roots(|value| check_value(value, "deferred lane"));
    vm.loader
        .forward_statics(&mut |value| check_value(value, "loader static"));
    // Explicit Vm fallback walk (contract); registered worker roots checked separately.
    for (index, value) in vm.gc_protect.iter().enumerate() {
        check_value(value, &format!("gc_protect[{index}]"));
    }
    unsafe {
        crate::vm::thread_context::with_registered_gc_protect_roots_mut(None, |roots| {
            for (index, value) in roots.iter().enumerate() {
                check_value(value, &format!("worker gc_protect[{index}]"));
            }
        });
    }

    crate::frames::native_args_gather(&mut |value| check_value(value, "native argument"));
    crate::arena_map::forward_temps(&mut |value| check_value(value, "arena-map temporary"));

    for &location in heap_value_locs {
        check_value(unsafe { &*location }, "arena heap Value cell");
    }
    for &location in heap_cell_locs {
        check_cell(unsafe { *location }, "arena heap cell");
    }
    for &location in cell_locs {
        check_cell(unsafe { *location }, "registered JIT/pinned cell");
    }
    // Re-enumerate the complete cell population, not only cells selected for this
    // collection's writeback. This finds an omitted nursery pointer in a slot the
    // minor-GC prefilter would otherwise have left out.
    let (all_heap_values, all_heap_cells) = vm.heap.arena_root_locations(false);
    for location in all_heap_values {
        check_value(unsafe { &*location }, "full arena heap Value scan");
    }
    for location in all_heap_cells {
        check_cell(unsafe { *location }, "full arena heap cell scan");
    }

    for (base, len) in crate::frames::cells_all() {
        let cells = unsafe { std::slice::from_raw_parts(base as *const i64, len) };
        for (index, cell) in cells.iter().enumerate() {
            check_cell(*cell, &format!("JIT cell buffer[{index}]"));
        }
    }
    if crate::jit::runtime::stackmaps::declaring() {
        crate::jit::runtime::stackmaps::for_each_map_slot(&mut |cell| {
            check_cell(unsafe { *cell }, "compiled stack-map slot");
        });
    }
    if crate::nextgen::pinned_mirror_enabled() {
        crate::nextgen::pinned_mirror_ref_cells(|cell| {
            check_cell(unsafe { *cell }, "pinned-mirror cell");
        });
    }
    if crate::nextgen::pinned_enum_enabled() {
        crate::nextgen::pinned_region_root_walk(|cell| {
            check_cell(unsafe { *cell }, "pinned-region cell");
        });
    }
}
