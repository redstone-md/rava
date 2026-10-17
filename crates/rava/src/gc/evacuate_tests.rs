//! Production forwarding with a destination page that cannot be replaced.

use super::*;
use crate::gc::large::LargeArea;
use crate::gc::space::{class_of, CLASSES, PAGE_SIZE};
use crate::klass::KlassParts;
use crate::symbol::Symbol;
use std::alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout};
use std::ptr::NonNull;

struct Pages(NonNull<u8>, Layout);

impl Pages {
    fn new() -> Self {
        let layout = Layout::from_size_align(3 * PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: a nonzero valid layout; the allocation stays alive through the entire test.
        let raw = unsafe { alloc_zeroed(layout) };
        Self(NonNull::new(raw).unwrap_or_else(|| handle_alloc_error(layout)), layout)
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        // SAFETY: this allocation was created with the same layout and no object still uses it.
        unsafe { dealloc(self.0.as_ptr(), self.1) };
    }
}

#[test]
fn evacuation_exhaustion_retires_promotion_run_once() {
    const CELL: usize = 32 << 10;
    let pages = Pages::new();
    let base = pages.0.as_ptr() as usize;
    let end = base + 3 * PAGE_SIZE;
    let class = class_of(CELL);
    let meta = |alloc, old| Some(PageMeta {
        class: class as u8, cell_size: CELL as u32,
        recip: ((1u64 << 40) + CELL as u64 - 1) / CELL as u64,
        alloc: vec![alloc].into_boxed_slice(), marks: vec![0].into_boxed_slice(),
        cursor: 0, owned: false, old,
    });
    let mut old_partial: [Vec<usize>; CLASSES] = std::array::from_fn(|_| Vec::new());
    old_partial[class].push(2);
    let mut space = Space {
        base, end, reserved: 3 * PAGE_SIZE, fresh: end,
        pages: vec![meta(0xff, false), meta(0x0f, false), meta(0, true)],
        empty: Vec::new(), released: Vec::new(), partial_free: 0,
        partial: std::array::from_fn(|_| Vec::new()), old_partial,
        large: Default::default(), large_bytes: 0, large_area: LargeArea::new(end, end),
        allocated_since_gc: 0, live_after_gc: 0, live_after_full: 0, aging: Vec::new(), tenure: super::super::mark::TENURE,
    };
    let klass = crate::klass::leak(Klass::new(KlassParts {
        name: Symbol::intern("EvacuationExhaustion"), kind: KlassKind::Instance, access: 0,
        super_klass: None, interfaces: Vec::new(), cf: None, fields: Vec::new(),
        instance_size: CELL as u32, ref_offsets: Vec::new(), static_slots: 0,
        initialized: true, hidden: false,
    }));
    let source: Vec<usize> = (0..12).map(|i| {
        let at = base + i * CELL;
        // SAFETY: each cell is zeroed, aligned, allocated in its page, and large enough.
        unsafe {
            let o = crate::heap::init_instance(at as *mut u8, klass);
            obj::mark(o).store((i as u64 + 1) << 8, Ordering::Relaxed);
            *(at as *mut u64).add(2) = i as u64;
        }
        at
    }).collect();
    let promote: *mut Space = &mut space;
    let shared = Shared {
        // SAFETY: one marker owns the fixture; production old_run changes only allocation state.
        space: unsafe { &*promote }, evacuate: true, promote: Mutex::new(promote),
        pool: Mutex::new(Vec::new()), pooled: AtomicUsize::new(0), idle: AtomicUsize::new(0),
        workers: 1, marked: AtomicUsize::new(0), promoted: Mutex::new(Vec::new()),
        young_again: &[], pinned: Mutex::new(Vec::new()),
        by_stacks: AtomicUsize::new(0), by_roots: AtomicUsize::new(0),
    };
    let mut runs = super::super::evacuate::Runs::default();
    let mut found = Vec::new();
    for (i, &at) in source.iter().enumerate() {
        let slot = AtomicU64::new(at as u64);
        shared.update_static(&slot as *const AtomicU64 as usize, &mut found, &mut runs);
        let now = slot.load(Ordering::Relaxed) as usize;
        assert_eq!(now != at, i < 8, "source {i}");
        // SAFETY: both the moved destination and the retained source are initialized live objects.
        unsafe {
            assert!(std::ptr::eq(obj::klass(now as Oop), klass));
            assert_eq!(obj::read::<u64>(now as Oop, 16), i as u64);
            assert_eq!(obj::mark(now as Oop).load(Ordering::Relaxed), (i as u64 + 1) << 8);
        }
        if i >= 8 {
            assert_eq!(shared.space.pages[1].as_ref().unwrap().marks[0] & (1 << (i - 8)), 1 << (i - 8));
        }
    }
    shared.finish_runs(&runs);
    assert_eq!(*shared.promoted.lock().unwrap(), vec![(base + 2 * PAGE_SIZE, end)]);
    assert_eq!(shared.space.pages[2].as_ref().unwrap().marks[0], 0xff);
    assert_eq!(shared.space.pages[1].as_ref().unwrap().marks[0], 0x0f);
    assert_eq!(found.len(), 12);
}
