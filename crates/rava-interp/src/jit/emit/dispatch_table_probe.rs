//! The direct-mapped dispatch-table probe.
//!
//! Runs only when the N-way PIC chain came up empty. One class-id-indexed load against a
//! table that is grown by publication, never mutated in place -- a reader can observe the old
//! table, the new one, or the pointer mid-swap, and an empty slot simply falls to the host.

use crate::jit::*;

#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_table_probe(
    b: &mut FunctionBuilder,
    fptr: Value,
    cid: Value,
    recv: Value,
    cache_ptr: i64,
    hit: cranelift::prelude::Block,
) -> Value {
    // DIRECT-MAPPED TABLE PROBE — increment 3 of docs/dispatch-direct-mapped-plan.md, and the
    // first one that changes emitted code.
    //
    // ⚠**THE GATE NAME AND POLARITY HERE WERE STALE AND SAID THE OPPOSITE OF THE TRUTH.** This
    // read "`RAVA_DISPATCH_TABLE`, default OFF" — there is no such variable, and
    // `dispatch_table::enabled()` is `RAVA_NO_DISPATCH_TABLE`-opt-OUT, i.e. **DEFAULT ON**. An
    // audit that trusts the header concludes this whole emitter is dead code in a product boot,
    // which is exactly backwards: every compiled arena dispatch site carries it.
    //
    // Runs ONLY when the N-way chain above came up empty, which on this boot is 58.8% of
    // 479,615,668 dynamic calls. Each of those is currently a ~99ns host callback, so serving
    // them from a class-id-indexed load is the whole lever: ~282M misses is ~29s of pure
    // transition, more than the entire ring window.
    //
    // ⭐**SHOULD IT BE EMITTED AT ALL? YES — AND THE CENSUS THAT SAYS OTHERWISE IS A DIFFERENT
    // TABLE'S.** The reading `table_hit=8,091` against `direct_hit=174,821,132` belongs to the
    // FREEZE table (`gilfree_cf::stats`), not to this probe.
    // below, and their recorded reading is the opposite shape: reached on 1,572,526 of
    // 289,918,574 misses (0.54%) and **hitting 96.2% of the time when reached**. So the probe is
    // not a miss machine that should be deleted — it is a near-perfect one that is rarely on the
    // path, and deleting it would push those serves back onto ~99ns host callbacks.
    //
    // WHERE ITS POPULATION ACTUALLY GOES, from the code rather than a guess. Two upstream
    // refusals starve it, and neither lives here:
    //   * `note_serve_direct`'s `shift` clause refuses every lambda entry while
    //     `RAVA_LAMBDA_SITE_CID` is unset (the default), and its own counter prices that at
    //     **19,492,939 of 19,492,939 serves** — the table's entire would-be population. The `cid`
    //     remap below is gated on the SAME flag, so the two halves are consistent; what is
    //     missing is the admission side, not this code.
    //   * a non-arena receiver never reaches `pic` at all (see the `emit_is_arena` branch in
    //     `emit_dispatch_pic`), so it cannot reach this probe either.
    // Both are somebody else's line. The only cost this file could remove is the `have4`
    // compare-and-branch, which duplicates the caller's own `fptr != 0` test one block later —
    // and collapsing that pair requires `hit` to take a block param, i.e. an edit to
    // `emit_dispatch_pic`, not to this emitter. Left alone deliberately.
    //
    // The "32.8 KB per site allocated before any verdict existed" charge is also already
    // answered: `dispatch_table::fast_dcache` (default ON) allocates on the FIRST FILL, and an
    // empty table and no table are the same answer to the three loads below.
    //
    // Three guarded loads and no compares against keys — a megamorphic site cannot evict
    // itself out of this table, because every class owns its own slot.
    //
    // The table pointer is read from the BAKED GLOBAL `cache_ptr`, never `cache_eff`: under
    // IC_PARALLEL `cache_eff` is a per-thread 16-way slice whose word 8 is a real PIC way.
    // (`dispatch_table::note` refuses to fill under IC_PARALLEL for the same reason, so the
    // slot is reliably 0 there and this probe just falls through.)
    //
    // CONCURRENT GROWTH IS SAFE. A grow leaks the old table and publishes a new pointer with
    // one aligned store. A reader can observe the old table, the new one, or the pointer
    // mid-swap; the old table is immortal and its entries stay valid, the new one starts empty
    // and an empty slot simply falls to the host — which is the status quo, never a wrong
    // method. That is the same monotonic-publication argument `freeze_resolve` already relies
    // on, and it is why growth does not need a lock on the read side.
    if dispatch_table::enabled() {
        let tprobe = b.create_block();
        let tbounds = b.create_block();
        let tload = b.create_block();
        let decide = b.create_block();
        b.append_block_param(decide, I64);
        let zero = b.ins().iconst(I64, 0);
        let have4 = b.ins().icmp_imm(IntCC::NotEqual, fptr, 0);
        let f4_args: [cranelift::codegen::ir::BlockArg; 1] = [fptr.into()];
        b.ins().brif(have4, decide, &f4_args, tprobe, &[]);

        let zero_args: [cranelift::codegen::ir::BlockArg; 1] = [zero.into()];
        b.switch_to_block(tprobe);
        b.seal_block(tprobe);
        // LAMBDA TABLE KEY, computed HERE — inside the arm the 4-way PIC has already missed.
        //
        // A lambda receiver must key by its impl SITE: every lambda of one capture signature
        // shares a class id, so a class-keyed hit would enter ANOTHER site's body. The site index
        // is the second inline cell (`AL_SITE_SLOT`), and `is_lambda_cid` is a bitmap this loads
        // one word of. `nextgen::lambdas::lambda_table_key` is the Rust twin that fills the table;
        // the two must compute the same number, so the Rust side is one function and this is its
        // transcription in the same order.
        //
        // ⚠**PLACEMENT IS THE WHOLE COST.** The first version computed this before `probe_ways`,
        // i.e. on every dispatch rather than only on a PIC miss: ChunkFill **4,589 → 60,282 ms**,
        // hashes bit-exact throughout. A gate cannot see a defect that is only slow.
        #[cfg(feature = "next-gen-gc")]
        let cid = if crate::nextgen::lambdas::site_cid_enabled() {
            let base = b
                .ins()
                .iconst(I64, crate::nextgen::lambdas::lambda_cid_bitmap_addr());
            let widx = b.ins().ushr_imm(cid, 6);
            let woff = b.ins().imul_imm(widx, 8);
            let waddr = b.ins().iadd(base, woff);
            let word = b.ins().load(I64, MemFlags::trusted(), waddr, Offset32::new(0));
            let sh = b.ins().band_imm(cid, 63);
            let shifted = b.ins().ushr(word, sh);
            let bit = b.ins().band_imm(shifted, 1);
            let is_lam = b.ins().icmp_imm(IntCC::NotEqual, bit, 0);
            let site_off = crate::nextgen::HDR as i32 + 8;
            let site = b.ins().load(I64, MemFlags::trusted(), recv, Offset32::new(site_off));
            let lam_key = b
                .ins()
                .iadd_imm(site, crate::nextgen::lambdas::LAMBDA_KEY_BASE as i64);
            b.ins().select(is_lam, lam_key, cid)
        } else {
            cid
        };
        let slot = b.ins().iconst(I64, cache_ptr + DISPATCH_TABLE_SLOT as i64);
        let tbl = b.ins().load(I64, MemFlags::trusted(), slot, Offset32::new(0));
        let has_tbl = b.ins().icmp_imm(IntCC::NotEqual, tbl, 0);
        b.ins().brif(has_tbl, tbounds, &[], decide, &zero_args);

        // Bounds-check against the table's OWN capacity word rather than a baked constant, so a
        // grow cannot be raced into an out-of-bounds read: cap is read from whichever table
        // `tbl` points at.
        b.switch_to_block(tbounds);
        b.seal_block(tbounds);
        let cap = b.ins().load(I64, MemFlags::trusted(), tbl, Offset32::new(0));
        let in_range = b.ins().icmp(IntCC::UnsignedLessThan, cid, cap);
        b.ins().brif(in_range, tload, &[], decide, &zero_args);

        b.switch_to_block(tload);
        b.seal_block(tload);
        let off = b.ins().imul_imm(cid, 8);
        let addr = b.ins().iadd(tbl, off);
        // +8 skips the capacity word; entry i lives at tbl + 8 + i*8.
        let f2 = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(8));
        // Inline, BRANCHLESS census of what this probe actually served (only when picprobe is
        // armed, so the production path pays nothing). `f2 != 0` widens to 0/1 and is added, so
        // a hit and a miss cost the same instructions and no control flow is introduced —
        // otherwise the counter would perturb the very thing it measures.
        if crate::picprobe::enabled() {
            let nz = b.ins().icmp_imm(IntCC::NotEqual, f2, 0);
            let nzi = b.ins().uextend(I64, nz);
            let ha = b.ins().iconst(I64, dispatch_table::dt_hit_addr());
            let hcur = b.ins().load(I64, MemFlags::trusted(), ha, Offset32::new(0));
            let hnew = b.ins().iadd(hcur, nzi);
            b.ins().store(MemFlags::trusted(), hnew, ha, Offset32::new(0));
            let pa = b.ins().iconst(I64, dispatch_table::dt_probe_addr());
            let pcur = b.ins().load(I64, MemFlags::trusted(), pa, Offset32::new(0));
            let pnew = b.ins().iadd_imm(pcur, 1);
            b.ins().store(MemFlags::trusted(), pnew, pa, Offset32::new(0));
        }
        let f2_args: [cranelift::codegen::ir::BlockArg; 1] = [f2.into()];
        b.ins().jump(decide, &f2_args);

        // A block param keeps `hit` at exactly ONE predecessor, so the deep/leaf decode below
        // still reads a dominating SSA value and needs no change at all.
        b.switch_to_block(decide);
        b.seal_block(decide);
        b.block_params(decide)[0]
    } else {
        fptr
    }
}
