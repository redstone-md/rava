//! Static value cells and the generation mirror over them.

use crate::heap::*;

impl Heap {
    /// Allocate the global getstatic value-cell table once (before any compiled code
    /// runs) and publish its base. Fixed size → base never moves. Each cell's gen
    /// half is the sentinel `-1` (u64::MAX), which never equals a real `static_gen`
    /// (a small counter) → every cell forces the callback on first access.
    pub fn ensure_static_cells(&mut self) {
        if self.static_cells.is_empty() {
            self.static_cells = vec![0i64; JIT_STATIC_SLOTS * 2];
            for s in 0..JIT_STATIC_SLOTS {
                self.static_cells[s * 2] = -1; // sentinel gen
            }
            self.jit_ctx.static_cells_base = self.static_cells.as_ptr() as usize;
        }
    }

    /// Refresh the native mirror of `Vm.static_gen` (called on every bump).
    /// Stage 2c transitional: `jit_ctx.static_gen` is the same repr(C)-mirrored
    /// `u64` as `jit_ctx.objects_base` — every write needs `&mut self` only
    /// because `Heap.jit_ctx: JitCtx` is plain data under the fixed machine-code
    /// offsets. The 4 `objects_base` sites are already absorbed; this one keeps
    /// the same `as_mut_transitional` discipline until JitCtx moves atomic.
    #[allow(invalid_reference_casting)]
    pub fn set_static_gen_mirror(&self, gen: u64) {
        unsafe { &mut *(self as *const Self as *mut Self) }.jit_ctx.static_gen = gen;
    }
    /// Reserve `n` consecutive getstatic cells for the getstatic sites of one
    /// compiling method, returning the first slot, or `None` when the fixed table
    /// can't fit them (those sites then keep using the callback). The caller must
    /// `free_static_slots(n)` if the compile ultimately fails (see the rollback in
    /// `jit_compile`) so rejected methods don't leak the table toward exhaustion.
    pub(crate) fn reserve_static_slots(&mut self, n: u32) -> Option<u32> {
        let start = self.next_static_slot;
        if (start as usize).checked_add(n as usize)? <= JIT_STATIC_SLOTS {
            self.next_static_slot += n;
            Some(start)
        } else {
            None
        }
    }
    /// The reservation high-water mark, taken BEFORE a compile reserves anything so the
    /// rollback can prove the block is still its own. See `free_static_slots_from`.
    pub(crate) fn static_slot_mark(&self) -> u32 {
        self.next_static_slot
    }

    /// Roll back a compile's reservation, but ONLY if it is still on top.
    ///
    /// ⚠ THIS REPLACES A LIFO ROLLBACK WHOSE INVARIANT WAS FALSE. The old
    /// `free_static_slots(n)` subtracted `n` from the high-water mark and documented why that
    /// was safe: *"compile path is single-threaded, so nothing else allocated in between → the
    /// top n slots are exactly ours"*. That holds only while ONE compile is in flight, which is
    /// what the background-JIT queue's depth of 1 quietly guaranteed. Raise the depth and two
    /// compiles interleave their reservations: the first to fail rolls the mark back THROUGH the
    /// second's live block, which is then handed out again to a third method.
    ///
    /// Measured, three 2-second runs (ledger row 194): background depth 32 fails to start the
    /// server; the SAME methods compiled synchronously boot; background depth 32 with
    /// `RAVA_NO_REF_STATIC_CELLS` boots. The failure signature is a compiled body reading a
    /// constant-pool index against another class's pool — i.e. cells owned by someone else.
    ///
    /// If another reservation landed on top of ours we simply leak: only a FAILED compile ever
    /// rolls back, and a boot has ~zero of those, so leaking is free where double-handing cells
    /// is a silent miscompile.
    pub(crate) fn free_static_slots_from(&mut self, mark: u32, n: u32) {
        if n > 0 && self.next_static_slot == mark.saturating_add(n) {
            self.next_static_slot = mark;
        }
    }

    /// Roll back the last `reserve_static_slots(n)` (compile path is single-threaded,
    /// so nothing else allocated in between → the top `n` slots are exactly ours).
    pub(crate) fn free_static_slots(&mut self, n: u32) {
        self.next_static_slot = self.next_static_slot.saturating_sub(n);
    }

    /// Populate a getstatic cell from the callback: `gen` is the CURRENT static_gen
    /// (so the next native read with a matching mirror hits), `packed` the value.
    pub(crate) fn write_static_cell(&mut self, slot: u32, gen: u64, packed: i64) {
        crate::Vm::debug_assert_not_gilfree("Heap::write_static_cell");
        let i = slot as usize * 2;
        if i + 1 < self.static_cells.len() {
            self.static_cells[i] = gen as i64;
            self.static_cells[i + 1] = packed;
        }
    }
}
