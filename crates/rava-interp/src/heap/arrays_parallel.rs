//! Arrays under a parallel region: reservation, worker writes, and the commit.

use crate::heap::*;
use crate::heap::arraydata::fast_arraydata;

/// Carry-over headroom the region reservation leaves on the three array spines, in SLOTS.
///
/// Sized against the failure it exists to stop (see `parallel_array_reserve`): the spine only
/// grows by the arrays a region KEEPS, so `slack / kept-per-region` regions run between
/// reallocations — hundreds of regions for a chunk batch that keeps tens of arrays.
///
/// **Total: 8192 slots x (ArrayObj 32B + ArrayDesc 16B + alive 1B) ~= 400 KB across all three
/// spines**, and it is CARRY-OVER, not retention: `Heap::trim_table_capacities` already shrinks
/// `arrays`/`array_descs` back the moment no region is open. Deliberately NOT "double the Vec" —
/// geometric growth of the whole spine is what retained ~96 MiB here before.
const PARR_SPINE_SLACK: usize = 8192;

/// The placeholder descriptor a reserved-but-unclaimed slot holds. A `const`, so the bulk fill
/// stores a compile-time constant instead of rebuilding the same struct once per slot.
const UNCLAIMED_ARRAY_DESC: ArrayDesc =
    ArrayDesc { data: std::ptr::null(), len: 0, element_id: DEAD_ARRAY_ELEMENT_ID };

impl Heap {
    /// G3.5 — open a PARALLEL ARRAY REGION: pre-extend all array spines by `n`
    /// placeholder slots (so worker writes never push/realloc), and arm the atomic slot counter.
    /// Returns the base handle. Workers then call `worker_array_put` (concurrent, &self); the
    /// driver closes with `parallel_array_commit`. Primitive arrays only (ref arrays need the
    /// &mut box table). Called under the GIL before the fork.
    pub fn parallel_array_reserve(&mut self, n: usize) -> u32 {
        let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
        let mut arr_alive = self.arr_alive.write().unwrap_or_else(|e| e.into_inner());
        let mut array_descs = self.array_descs.write().unwrap_or_else(|e| e.into_inner());

        let trace_caps = crate::parallel::fork_trace_enabled().then_some((
            arrays.capacity(),
            arr_alive.capacity(),
            array_descs.capacity(),
        ));
        // Pre-intern the 8 primitive element names once (atype 4..=11) so workers never intern.
        if self.parr_prim_element_ids.is_empty() {
            for name in ["boolean", "char", "float", "double", "byte", "short", "int", "long"] {
                let id = self.intern_array_element(name);
                self.parr_prim_element_ids.push(id);
            }
        }
        let base = arrays.len();
        let new_len = base + n;
        let slack = if fast_arraydata() { n.min(PARR_SPINE_SLACK) } else { 0 };
        if arrays.capacity() < new_len {
            arrays.reserve_exact(n + slack);
        }
        if arr_alive.capacity() < new_len {
            arr_alive.reserve_exact(n + slack);
        }
        if array_descs.capacity() < new_len {
            array_descs.reserve_exact(n + slack);
        }
        if fast_arraydata() {
            arrays.resize_with(new_len, || ArrayObj { data: ArrayData::Int(Vec::new()) });
            arr_alive.resize(new_len, false);
            array_descs.resize(new_len, UNCLAIMED_ARRAY_DESC);
        } else {
            for _ in 0..n {
                arrays.push(ArrayObj { data: ArrayData::Int(Vec::new()) });
                arr_alive.push(false);
                array_descs.push(UNCLAIMED_ARRAY_DESC);
            }
        }
        self.jit_ctx.desc_base = array_descs.as_ptr() as usize;
        self.parr_base = base;
        self.parr_cap = base + n;
        self.parr_next = std::sync::atomic::AtomicUsize::new(base);
        self.parr_epoch = NEXT_PARALLEL_ARRAY_EPOCH.fetch_add(
            1,
            std::sync::atomic::Ordering::Relaxed,
        );
        if let Some(before) = trace_caps {
            eprintln!(
                "[PARR-RESERVE] slots={} len={} caps={:?}->{:?} slot_bytes={}",
                n,
                arrays.len(),
                before,
                (arrays.capacity(), arr_alive.capacity(), array_descs.capacity()),
                std::mem::size_of::<ArrayObj>()
                    + std::mem::size_of::<bool>()
                    + std::mem::size_of::<ArrayDesc>(),
            );
        }
        base as u32
    }

    /// G3.5 — a fork WORKER claims the next pre-reserved array slot and writes it (element +
    /// primitive data) via raw pointers. `&self` + a UNIQUE atomic index per call → disjoint
    /// writes into the never-reallocated pre-reserved slabs, so this is race-free across workers.
    /// Returns the array handle, or `None` when the region is exhausted (→ region abort → serial).
    /// `element_id` must be pre-interned; workers never mutate the owning interner.
    pub fn worker_array_put(&self, element_id: u32, data: ArrayData) -> Option<u32> {
        let idx = self.worker_array_slot()?;
        // ONE discriminant switch, not two: `data_ptr()` and `len()` read two adjacent fields of
        // the SAME `Vec` header, and this runs once per array a worker publishes.
        let (dptr, dlen) = if fast_arraydata() {
            let (p, l) = data.ptr_len();
            (p, l as u32)
        } else {
            (data.data_ptr(), data.len() as u32)
        };
        // G3.5 lock-free worker write: take SHARED read guards (workers do not serialize) purely
        // to read the stable spine base pointers. The region pre-reserved every slot, so no push
        // and no realloc can move these Vecs while a region is open, and each worker owns a
        // disjoint `idx` — so writing through *const→*mut under a shared guard races nothing.
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let array_descs = self.array_descs.read().unwrap_or_else(|e| e.into_inner());
        let arr_alive = self.arr_alive.read().unwrap_or_else(|e| e.into_inner());
        unsafe {
            // Disjoint pre-reserved slots, no realloc → writing through *const→*mut is sound.
            // `*slot =` drops the placeholder ArrayObj's empty Vec — done once per idx.
            let ao = (arrays.as_ptr() as *mut ArrayObj).add(idx);
            *ao = ArrayObj { data };
            *(array_descs.as_ptr() as *mut ArrayDesc).add(idx) = ArrayDesc {
                data: dptr,
                len: dlen,
                element_id,
            };
            *(arr_alive.as_ptr() as *mut bool).add(idx) = true;
        }
        Some(idx as u32)
    }

    /// Claim one slot from this execution thread's current range. A single shared RMW reserves
    /// several consecutive handles; subsequent allocations touch only TLS until the range is
    /// empty. Heap identity plus a globally unique region epoch makes stale TLS fail closed.
    pub(crate) fn worker_array_slot(&self) -> Option<usize> {
        if self.parr_cap == 0 {
            return None;
        }
        let heap = self as *const Self as usize;
        crate::heap::WORKER_ARRAY_CURSOR.with(|cell| {
            let mut cursor = cell.get();
            let cached = cursor.heap == heap
                && cursor.epoch == self.parr_epoch
                && cursor.next >= self.parr_base
                && cursor.next < cursor.end
                && cursor.end <= self.parr_cap;
            if !cached {
                let claim = self.parr_next.fetch_update(
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                    |next| {
                        (next < self.parr_cap).then_some(
                            next.saturating_add(PARALLEL_ARRAY_CLAIM_CHUNK).min(self.parr_cap),
                        )
                    },
                );
                let Ok(start) = claim else {
                    cell.set(WorkerArrayCursor {
                        heap,
                        epoch: self.parr_epoch,
                        next: self.parr_cap,
                        end: self.parr_cap,
                    });
                    return None;
                };
                cursor = WorkerArrayCursor {
                    heap,
                    epoch: self.parr_epoch,
                    next: start,
                    end: start.saturating_add(PARALLEL_ARRAY_CLAIM_CHUNK).min(self.parr_cap),
                };
            }
            let idx = cursor.next;
            cursor.next += 1;
            cell.set(cursor);
            Some(idx)
        })
    }

    /// Is a parallel array region currently open (workers may claim slots)?
    #[inline]
    pub fn parallel_array_active(&self) -> bool {
        self.parr_cap != 0
    }

    /// Was array `h` allocated INSIDE the currently-open parallel array region?
    ///
    /// This is the per-array ownership record the GILFREE-RINGS array-store barrier
    /// lacked. Slots `[parr_base, parr_cap)` are reserved fresh for THIS region, so an
    /// array answering true cannot be part of the pre-existing shared object graph —
    /// it was allocated by a worker during this region, from its own claim chunk, and
    /// nothing outside the region can reach it (a task's only inputs are its captures,
    /// which predate the region). Writing such an array off-GIL therefore races
    /// nothing: each array owns its own data buffer, and the descriptor spine does not
    /// move while a region is open. A false answer means "shared graph, or no region"
    /// — the caller must deopt.
    #[inline]
    pub fn array_in_parallel_region(&self, h: u32) -> bool {
        let idx = h as usize;
        self.parr_cap != 0 && idx >= self.parr_base && idx < self.parr_cap
    }

    /// G3.5 — a fork worker's `newarray atype[count]`: build the zero-filled primitive data and
    /// claim a pre-reserved slot (pre-interned element, no `intern`). `None` → region exhausted
    /// or a non-primitive atype (→ abort). Concurrent-safe (`worker_array_put` is `&self`).
    pub fn worker_newarray(&self, atype: i64, count: usize) -> Option<u32> {
        let data = match atype {
            4 | 8 => ArrayData::Byte(vec![0u8; count]),      // boolean / byte
            5 | 9 | 10 => ArrayData::Int(vec![0i32; count]), // char / short / int
            11 => ArrayData::Long(vec![0i64; count]),
            6 => ArrayData::Float(vec![0f32; count]),
            7 => ArrayData::Double(vec![0f64; count]),
            _ => return None,
        };
        let element_id = *self.parr_prim_element_ids.get((atype as usize).checked_sub(4)?)?;
        self.worker_array_put(element_id, data)
    }

    /// GAP-S09 stage 3 — a fork worker's `anewarray` inside an OPEN parallel array
    /// region: resolve the element type READ-ONLY (`array_element_ids` lookup, NO
    /// insert — the interner is frozen during a region, so a hit is race-free) and
    /// claim a pre-reserved slot (`worker_array_put`, `&self`, concurrent-safe).
    /// `None` → element type never interned (cold) or region exhausted → the caller
    /// aborts the region. Mirrors `worker_newarray` (the primitive twin), including
    /// skipping the serial-path `bytes` tally. `element` is the descriptor form the
    /// serial path interns (`L<name>;`), so a hit yields the IDENTICAL element id —
    /// bit-exact array metadata either way.
    pub fn worker_anewarray_ref(&self, element: &str, count: usize) -> Option<u32> {
        let id = *self.array_element_ids.read().unwrap_or_else(|e| e.into_inner()).get(element)?;
        self.worker_array_put(id, ArrayData::Ref(vec![0i64; count]))
    }

    /// G3.5 — close the parallel array region after every scoped worker joined. Trim the wholly
    /// unused suffix and return unpublished holes from partial worker ranges to `free_arr`.
    /// Returns the exact number of arrays allocated, not the cursor high-water mark.
    pub fn parallel_array_commit(&mut self) -> usize {
        let raw_next = self.parr_next.load(std::sync::atomic::Ordering::Relaxed);
        let base = self.parr_base;
        let claimed_end = raw_next.min(self.parr_cap).max(base);
        let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
        let mut arr_alive = self.arr_alive.write().unwrap_or_else(|e| e.into_inner());
        let mut array_descs = self.array_descs.write().unwrap_or_else(|e| e.into_inner());
        let mut free_arr = self.free_arr.lock().unwrap_or_else(|e| e.into_inner());

        let span = fast_arraydata().then(|| arr_alive.get(base..claimed_end)).flatten();
        let (count, keep_end) = if let Some(span) = span {
            let keep_len = span.iter().rposition(|&alive| alive).map_or(0, |p| p + 1);
            (span[..keep_len].iter().filter(|&&alive| alive).count(), base + keep_len)
        } else {
            let mut count = 0;
            let mut keep_end = base;
            for idx in base..claimed_end {
                if arr_alive[idx] {
                    count += 1;
                    keep_end = idx + 1;
                }
            }
            (count, keep_end)
        };
        for idx in base..keep_end {
            if !arr_alive[idx] {
                debug_assert!(array_descs[idx].data.is_null());
                debug_assert_eq!(array_descs[idx].len, 0);
                array_descs[idx] = UNCLAIMED_ARRAY_DESC;
                free_arr.push(idx as u32);
                handle_epoch_bump();
            }
        }
        arrays.truncate(keep_end);
        arr_alive.truncate(keep_end);
        array_descs.truncate(keep_end);
        self.jit_ctx.desc_base = array_descs.as_ptr() as usize;
        if crate::parallel::fork_trace_enabled() {
            eprintln!(
                "[PARR-COMMIT] epoch={} reserved={} claimed_span={} arrays={} interior_holes={} raw_next={}",
                self.parr_epoch,
                self.parr_cap - self.parr_base,
                claimed_end - self.parr_base,
                count,
                keep_end - self.parr_base - count,
                raw_next,
            );
        }
        self.parr_cap = 0;
        count
    }

    /// Snapshot the descriptor bases after the coordinator has completed all
    /// region reserves. The guard must be dropped before those bases can move.
    pub(crate) fn install_worker_jit_ctx(&self) -> WorkerJitCtxGuard {
        WorkerJitCtxGuard::install(self.jit_ctx)
    }

    /// M3 COW: arm/disarm the compiled field-access COW branch. Set to 1 strictly while a
    /// COW parallel phase-B region is live (workers isolate writes to their shadow); 0 makes
    /// compiled getfield/putfield take the inline fast path (main-thread boot speed).
    pub fn set_cow_armed(&mut self, on: bool) {
        self.jit_ctx.cow_armed = on as u32;
    }
}
