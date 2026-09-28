//! String operations emitted without a callback.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// ⭐**WIDER STRING EMISSION** — kill switch `RAVA_NO_EMIT_STRING=1`, and the
/// block-wide `RAVA_NO_EMIT_MORE` turns it off with everything else.
///
/// Consulted at EMIT time only. Every site that reads it emits the PREVIOUS
/// instruction sequence verbatim when it is off, so a run with the switch set
/// produces byte-identical machine code and the two can be A/B'd in one binary.
///
/// What it gates (all inside this file — no new call sites, no new callbacks):
///   * `startsWith`: an identity pre-test, and a 64-bit-word body compare in
///     place of the byte-at-a-time loop.
///   * inline concat: the Java hash folded WHILE the bytes are written (a literal
///     segment folds to two compile-time constants) instead of a second full pass
///     over the result, and literal bytes stored 8 at a time.
///
/// Costs if removed: `startsWith` goes back to one branch per byte compared and
/// loses the interned-identity answer; every inline-concat result is walked twice
/// and every literal byte costs its own `store.i8`.
#[cfg(feature = "next-gen-gc")]
fn emit_string_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_EMIT_STRING").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// STRING INTRINSIC (next-gen-gc): emit `recv.charAt(idx)` and push the char as
/// an I32. The fast arm needs the receiver to be an arena string that is pure
/// ASCII, because only then is the CHAR index also the BYTE index; it is then a
/// single `load.i8` + zero-extend with NO callback at all (the whole point — a
/// faster native would not have helped, only removing the call does).
///
/// The ASCII test is `blen == clen`. UTF-8 encodes every non-ASCII scalar in 2-4
/// bytes, so byte-length equals char-length exactly when every char is one byte;
/// this is the same `blen`/`clen` pair the `length()` intrinsic reads, so it
/// inherits that path's established layout contract. Anything else — a boxed
/// `Value::Str` receiver, a non-ASCII arena string, or an out-of-range index —
/// falls to `rava_str_charat`, which reproduces `str_dispatch`'s exact semantics
/// (out-of-range → 0, PUA surrogates unmapped).
///
/// ⛔**NOT WIDENED, DELIBERATELY.** The only shape left outside the fast arm is a
/// non-ASCII arena string, and there a char index is a UTF-8 DECODE from byte 0 —
/// an unbounded loop per `charAt`, which is exactly what the cold arm already does
/// and does once instead of once per site. Nothing here is "a load and a compare"
/// that is not already emitted.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_str_charat(b: &mut FunctionBuilder, env: &Env, recv: Value, idx: Value) -> Value {
    use crate::nextgen::{STR_JIT_BLEN_OFF, STR_JIT_BYTES_OFF, STR_JIT_CLEN_OFF};
    let check = b.create_block(); // arena receiver → ASCII + bounds test
    let fast = b.create_block(); // inline byte load
    let slow = b.create_block(); // callback
    let merge = b.create_block();
    b.append_block_param(merge, I32);

    let is_a = emit_is_arena(b, recv);
    b.ins().brif(is_a, check, &[], slow, &[]);

    // ── check: pure-ASCII (blen == clen) AND 0 <= idx < blen (unsigned compare,
    // so a negative index folds into the same test and takes the callback).
    b.switch_to_block(check);
    b.seal_block(check);
    let blen = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(STR_JIT_BLEN_OFF));
    let clen = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(STR_JIT_CLEN_OFF));
    let ascii = b.ins().icmp(IntCC::Equal, blen, clen);
    let in_bounds = b.ins().icmp(IntCC::UnsignedLessThan, idx, blen);
    let ok = b.ins().band(ascii, in_bounds);
    b.ins().brif(ok, fast, &[], slow, &[]);

    // ── fast: bytes are inline at `recv + STR_BYTES_OFF`; char index == byte index.
    b.switch_to_block(fast);
    b.seal_block(fast);
    let off = b.ins().uextend(I64, idx);
    let base = b.ins().iadd_imm(recv, STR_JIT_BYTES_OFF as i64);
    let addr = b.ins().iadd(base, off);
    let byte = b.ins().load(I8, MemFlags::trusted(), addr, Offset32::new(0));
    let ch = b.ins().uextend(I32, byte);
    b.ins().jump(merge, &[ch.into()]);

    // ── slow: thin callback (general value semantics, no synthetic frame).
    b.switch_to_block(slow);
    b.seal_block(slow);
    let idx64 = b.ins().sextend(I64, idx);
    let call = b.ins().call(env.str_charat_fn, &[recv, idx64]);
    let res = b.inst_results(call)[0];
    let r = b.ins().ireduce(I32, res);
    b.ins().jump(merge, &[r.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// STRING INTRINSIC (next-gen-gc): emit `recv.startsWith(prefix)` and push the
/// boolean (I32 0/1).
///
/// Routes to the word-compare body (default) or, with `RAVA_NO_EMIT_STRING` set,
/// to the original byte-at-a-time body which is kept verbatim below so the switch
/// is a true one-binary A/B.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_str_startswith(
    b: &mut FunctionBuilder,
    env: &Env,
    recv: Value,
    prefix: Value,
) -> Value {
    if emit_string_enabled() {
        emit_str_startswith_words(b, env, recv, prefix)
    } else {
        emit_str_startswith_bytes(b, env, recv, prefix)
    }
}

/// `startsWith` answered by identity, then compared eight bytes at a time.
///
/// **THE IDENTITY GUARD PROVES**: `nz & (recv == prefix)` — the two operand cells
/// are bit-equal and non-zero, i.e. the same non-null reference in the same
/// encoding, hence the same object. A string is always a prefix of itself, so the
/// answer is `true` with no memory touched at all. The `nz` conjunct is
/// load-bearing, not defensive: `null == null` must NOT be answered here, it must
/// keep reaching the callback so a null receiver behaves exactly as before. Arena
/// strings minted from the constant pool are interned, so identity is the common
/// answer for the `startsWith(<literal>)` tests the boot runs on resource ids.
///
/// **THE WORD GUARD PROVES**: the same precondition the byte loop used — both
/// operands are raw arena pointers (`emit_is_arena`: non-null, no JIT tag bits)
/// and `recv.blen >= prefix.blen` — is already enough for 8-byte steps, because
/// the object layout guarantees the rest. Inline bytes start at `STR_BYTES_OFF`
/// (= HDR + 16, and HDR is a multiple of 8, so the base is 8-aligned) and are
/// allocated in whole 8-byte cells (`nf = 2 + ceil(blen/8)`). So for any `i` that
/// is a multiple of 8 with `i < prefix.blen`, the eight bytes at `bytes+i` lie
/// inside BOTH objects' own storage: inside prefix's because
/// `i + 8 <= ceil(plen/8)*8`, and inside recv's because `plen <= rlen`. The loads
/// are therefore in-bounds AND aligned, which is what lets them keep
/// `MemFlags::trusted()`.
///
/// The compare is `(a ^ b) & mask`: `mask` is all-ones for a full word and
/// `(1 << 8*rem) - 1` for the final partial one, so the uninitialised pad bytes
/// past `plen` — which no reader ever looks at, since every reader bounds by
/// `byte_len` — cannot change the answer. `plen == 0` exits the loop immediately
/// with `true`, which is what `"x".startsWith("")` must return.
///
/// COLD ARM: `rava_str_startswith`, unchanged — a boxed `Value::Str` on either
/// side, or a null receiver. Cost if removed: one taken branch per byte compared,
/// and a full byte walk for pairs that are literally the same interned object.
#[cfg(feature = "next-gen-gc")]
fn emit_str_startswith_words(
    b: &mut FunctionBuilder,
    env: &Env,
    recv: Value,
    prefix: Value,
) -> Value {
    use crate::nextgen::{STR_JIT_BLEN_OFF, STR_JIT_BYTES_OFF};
    let kind = b.create_block(); // not the same object → test both kinds
    let fast = b.create_block(); // both arena → native word compare
    let slow = b.create_block(); // mixed / null → callback
    let wloop = b.create_block();
    b.append_block_param(wloop, I64); // byte index i, always a multiple of 8
    let wbody = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I32); // result 0/1

    // Constants minted in the entry block, which dominates every block below.
    let zero32 = b.ins().iconst(I32, 0);
    let one32 = b.ins().iconst(I32, 1);
    let zero64 = b.ins().iconst(I64, 0);

    // ── identity: same non-null cell → true, no loads.
    let nz = b.ins().icmp(IntCC::NotEqual, recv, zero64);
    let same = b.ins().icmp(IntCC::Equal, recv, prefix);
    let ident = b.ins().band(nz, same);
    b.ins().brif(ident, merge, &[one32.into()], kind, &[]);

    // ── kind: the original both-arena test.
    b.switch_to_block(kind);
    b.seal_block(kind);
    let ra = emit_is_arena(b, recv);
    let pa = emit_is_arena(b, prefix);
    let both = b.ins().band(ra, pa);
    b.ins().brif(both, fast, &[], slow, &[]);

    // ── fast: lengths, then the loop invariants (byte bases, plen widened once).
    b.switch_to_block(fast);
    b.seal_block(fast);
    let rlen = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(STR_JIT_BLEN_OFF));
    let plen = b.ins().load(I32, MemFlags::trusted(), prefix, Offset32::new(STR_JIT_BLEN_OFF));
    let plen64 = b.ins().uextend(I64, plen);
    let rbase = b.ins().iadd_imm(recv, STR_JIT_BYTES_OFF as i64);
    let pbase = b.ins().iadd_imm(prefix, STR_JIT_BYTES_OFF as i64);
    // recv.len >= prefix.len else not a prefix → false.
    let len_ok = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, rlen, plen);
    b.ins().brif(len_ok, wloop, &[zero64.into()], merge, &[zero32.into()]);

    // ── wloop(i): consumed the whole prefix → true; else compare the next word.
    b.switch_to_block(wloop);
    let i = b.block_params(wloop)[0];
    let done = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, plen64);
    b.ins().brif(done, merge, &[one32.into()], wbody, &[]);

    // ── wbody: one 8-byte compare, masked down to the bytes still inside prefix.
    b.switch_to_block(wbody);
    b.seal_block(wbody);
    let raddr = b.ins().iadd(rbase, i);
    let paddr = b.ins().iadd(pbase, i);
    let rw = b.ins().load(I64, MemFlags::trusted(), raddr, Offset32::new(0));
    let pw = b.ins().load(I64, MemFlags::trusted(), paddr, Offset32::new(0));
    let diff = b.ins().bxor(rw, pw);
    let rem = b.ins().isub(plen64, i); // >= 1 here, since i < plen
    let full = b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, rem, 8);
    // 8..56 on the partial arm (rem is 1..7 there); the value is dead when `full`.
    let shamt = b.ins().imul_imm(rem, 8);
    let one64 = b.ins().iconst(I64, 1);
    let bit = b.ins().ishl(one64, shamt);
    let partial = b.ins().iadd_imm(bit, -1);
    let allones = b.ins().iconst(I64, -1);
    let mask = b.ins().select(full, allones, partial);
    let masked = b.ins().band(diff, mask);
    let eq = b.ins().icmp_imm(IntCC::Equal, masked, 0);
    let inext = b.ins().iadd_imm(i, 8);
    b.ins().brif(eq, wloop, &[inext.into()], merge, &[zero32.into()]);
    b.seal_block(wloop); // both preds (fast, wbody) now emitted

    // ── slow: thin callback (general value semantics).
    b.switch_to_block(slow);
    b.seal_block(slow);
    let call = b.ins().call(env.str_startswith_fn, &[recv, prefix]);
    let res = b.inst_results(call)[0];
    let r = b.ins().ireduce(I32, res);
    b.ins().jump(merge, &[r.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// The pre-`RAVA_NO_EMIT_STRING` body, kept verbatim so the switch is a true A/B:
/// when both operands are arena strings, compare bytes inline (load both byte
/// lengths, bail if `recv.len < prefix.len`, then a byte-at-a-time equality loop
/// over `prefix.len` bytes) — zero callbacks. Otherwise a thin
/// `rava_str_startswith` callback handles the general value case.
#[cfg(feature = "next-gen-gc")]
fn emit_str_startswith_bytes(
    b: &mut FunctionBuilder,
    env: &Env,
    recv: Value,
    prefix: Value,
) -> Value {
    use crate::nextgen::{STR_JIT_BLEN_OFF, STR_JIT_BYTES_OFF};
    let fast = b.create_block(); // both arena → native byte compare
    let slow = b.create_block(); // mixed → callback
    let loop_head = b.create_block();
    b.append_block_param(loop_head, I64); // byte index i
    let loop_body = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I32); // result 0/1

    let ra = emit_is_arena(b, recv);
    let pa = emit_is_arena(b, prefix);
    let both = b.ins().band(ra, pa);
    b.ins().brif(both, fast, &[], slow, &[]);

    // ── fast: native byte compare. rlen,plen are byte lengths (i32).
    b.switch_to_block(fast);
    b.seal_block(fast);
    let rlen = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(STR_JIT_BLEN_OFF));
    let plen = b.ins().load(I32, MemFlags::trusted(), prefix, Offset32::new(STR_JIT_BLEN_OFF));
    let plen64 = b.ins().uextend(I64, plen);
    // recv.len >= prefix.len else not a prefix → false.
    let len_ok = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, rlen, plen);
    let zero32 = b.ins().iconst(I32, 0);
    let zero64 = b.ins().iconst(I64, 0);
    b.ins().brif(len_ok, loop_head, &[zero64.into()], merge, &[zero32.into()]);

    // ── loop_head(i): i == plen → all matched → true; else compare byte i.
    b.switch_to_block(loop_head);
    let i = b.block_params(loop_head)[0];
    let done = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, plen64);
    let one32 = b.ins().iconst(I32, 1);
    b.ins().brif(done, merge, &[one32.into()], loop_body, &[]);

    // ── loop_body: load recv[bytes+i] vs prefix[bytes+i]; mismatch → false; else i+1.
    b.switch_to_block(loop_body);
    b.seal_block(loop_body);
    let rbase = b.ins().iadd_imm(recv, STR_JIT_BYTES_OFF as i64);
    let pbase = b.ins().iadd_imm(prefix, STR_JIT_BYTES_OFF as i64);
    let raddr = b.ins().iadd(rbase, i);
    let paddr = b.ins().iadd(pbase, i);
    let rb = b.ins().load(I8, MemFlags::trusted(), raddr, Offset32::new(0));
    let pb = b.ins().load(I8, MemFlags::trusted(), paddr, Offset32::new(0));
    let eq = b.ins().icmp(IntCC::Equal, rb, pb);
    let i1 = b.ins().iadd_imm(i, 1);
    let z32 = b.ins().iconst(I32, 0);
    b.ins().brif(eq, loop_head, &[i1.into()], merge, &[z32.into()]);
    b.seal_block(loop_head); // both preds (fast, loop_body) now emitted

    // ── slow: thin callback (general value semantics).
    b.switch_to_block(slow);
    b.seal_block(slow);
    let call = b.ins().call(env.str_startswith_fn, &[recv, prefix]);
    let res = b.inst_results(call)[0];
    let r = b.ins().ireduce(I32, res);
    b.ins().jump(merge, &[r.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// The two compile-time constants that replace hashing a literal segment at run time.
///
/// The specified fold is `h = h*31 + c` per char. Over a run of `k` chars that is
/// exactly `h_k = h_0 * 31^k + sum(b_i * 31^(k-1-i))` — the left factor depends only
/// on the LENGTH and the right term only on the BYTES, and both are known when the
/// concat shape is baked. Returns `(31^k, sum)` in wrapping i32, which is the
/// arithmetic `String.hashCode` specifies. Sound only because concat literals are
/// proven ASCII when the shape is built (`concat_shape_at` bails on any char
/// >= 0x80), so byte value == char value.
#[cfg(feature = "next-gen-gc")]
fn lit_hash_coeffs(bytes: &[u8]) -> (i32, i32) {
    let mut pow: i32 = 1;
    let mut sum: i32 = 0;
    for &x in bytes {
        pow = pow.wrapping_mul(31);
        sum = sum.wrapping_mul(31).wrapping_add(x as i32);
    }
    (pow, sum)
}

/// Store a baked literal at `base` in 8-byte, then 4-byte, then 1-byte pieces.
///
/// Chunking is EXACT — it never writes past `bytes.len()`, because the byte after
/// the literal belongs to the next segment and would be clobbered. `base` carries a
/// running write offset that is not a multiple of 8 in general, so these are
/// unaligned stores and must NOT claim `MemFlags::trusted()`'s aligned bit;
/// `notrap` alone is what the object guarantees (the whole padded byte region
/// belongs to this just-bumped TLAB object).
#[cfg(feature = "next-gen-gc")]
fn emit_store_lit_chunked(b: &mut FunctionBuilder, base: Value, bytes: &[u8]) {
    let mf = MemFlags::new().with_notrap();
    let mut k = 0usize;
    while bytes.len() - k >= 8 {
        let mut w = 0u64;
        for j in 0..8 {
            w |= (bytes[k + j] as u64) << (8 * j);
        }
        let v = b.ins().iconst(I64, w as i64);
        b.ins().store(mf, v, base, Offset32::new(k as i32));
        k += 8;
    }
    if bytes.len() - k >= 4 {
        let mut w = 0u32;
        for j in 0..4 {
            w |= (bytes[k + j] as u32) << (8 * j);
        }
        let v = b.ins().iconst(I32, (w as i32) as i64);
        b.ins().store(mf, v, base, Offset32::new(k as i32));
        k += 4;
    }
    while k < bytes.len() {
        let cb = b.ins().iconst(I8, bytes[k] as i64);
        b.ins().store(mf, cb, base, Offset32::new(k as i32));
        k += 1;
    }
}

/// INLINE CONCAT (next-gen-gc): assemble a `makeConcat[WithConstants]` result whose
/// dynamic args are ALL `int` straight into a TLAB-bumped ArenaString — itoa each
/// int with a native divide-by-10 digit loop into a stack scratch, copy baked
/// literal bytes, fold the Java hash inline — with NO `rava_indy` callback. A bump
/// that doesn't fit the nursery falls back to the `env.indy` callback (which
/// collects + retries), so correctness never depends on the fast path. `args` are
/// the int operands in recipe push order; the result Ref is pushed on `stack`.
///
/// Layout written is byte-identical to `nextgen::tlab_alloc_string` (header
/// `[0]=class|nfields [8]=fwd=0 [16]=idhash=0`, `[24]=byte_len [28]=char_len
/// [32]=java_hash [40..]=UTF-8`). All literals are ASCII (verified at shape build)
/// so `char_len == byte_len`. The trailing pad of the final cell is left as-is
/// (never read: every reader bounds by `byte_len`; the hash folds only `[0..len)`;
/// ref_mask is empty so the collector never traces the bytes).
///
/// ⭐**ONE PASS OVER THE BYTES, NOT TWO** (`RAVA_NO_EMIT_STRING` restores the
/// second). The cached hash used to be a separate byte-at-a-time loop over the
/// finished result — a full extra walk of every string this path builds, on top of
/// the walk that wrote it. It is now folded as the bytes are produced: a literal
/// segment contributes `h*31^k + C` with both constants baked by `lit_hash_coeffs`
/// (ONE multiply and ONE add for a literal of any length), and an int segment folds
/// inside the digit-copy loop that was already touching each byte, off the byte it
/// has just loaded. The value stored at `STR_JIT_HASH_OFF` is the same specified
/// fold in the same left-to-right order, so `hashCode()` — which compiled code
/// reads as a bare `load.i32` — is unchanged, including 0 for an empty result.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_inline_concat(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    cursor_var: Variable,
    shape: &ConcatShape,
    args: &[SV],
    idx: u16,
) {
    use crate::nextgen::{STR_JIT_BLEN_OFF, STR_JIT_BYTES_OFF, STR_JIT_CLEN_OFF, STR_JIT_HASH_OFF};
    let fuse = emit_string_enabled();
    let n = shape.nargs;
    let cid = crate::nextgen::string_class_id() as i64;
    let lit_total: i64 = shape
        .segs
        .iter()
        .map(|s| match s {
            ConcatSeg::Lit(v) => v.len() as i64,
            ConcatSeg::IntArg(_) => 0,
        })
        .sum();

    // Scratch: n*16 bytes — each int arg's decimal digits written reverse into its
    // own 16-byte slot (11 chars max for an i32 incl. sign, rounded up).
    let scratch = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        (n as u32 * 16).max(8),
        3,
    ));
    let scratch_base = b.ins().stack_addr(I64, scratch, 0);

    // ── itoa each int arg into scratch (reverse); record (start_off, len). ──
    let mut starts: Vec<Value> = Vec::with_capacity(n);
    let mut lens: Vec<Value> = Vec::with_capacity(n);
    for (j, a) in args.iter().enumerate() {
        let end = b.ins().iconst(I64, (j as i64) * 16 + 16);
        let val64 = b.ins().sextend(I64, a.v);
        let zero = b.ins().iconst(I64, 0);
        let is_neg = b.ins().icmp(IntCC::SignedLessThan, val64, zero);
        let negval = b.ins().isub(zero, val64);
        let mag0 = b.ins().select(is_neg, negval, val64); // |val| (INT_MIN fits i64)

        // do-while digit loop: at least one digit (mag==0 → '0').
        let body = b.create_block();
        b.append_block_param(body, I64); // pos (write cursor, exclusive)
        b.append_block_param(body, I64); // mag
        let after = b.create_block();
        b.append_block_param(after, I64); // dstart (first digit offset)
        b.ins().jump(body, &[end.into(), mag0.into()]);

        b.switch_to_block(body);
        let pos = b.block_params(body)[0];
        let mag = b.block_params(body)[1];
        let pos1 = b.ins().iadd_imm(pos, -1);
        let q = b.ins().udiv_imm(mag, 10);
        let r = b.ins().urem_imm(mag, 10);
        let ch = b.ins().iadd_imm(r, '0' as i64);
        let chb = b.ins().ireduce(I8, ch);
        let waddr = b.ins().iadd(scratch_base, pos1);
        b.ins().store(MemFlags::trusted(), chb, waddr, Offset32::new(0));
        let more = b.ins().icmp_imm(IntCC::NotEqual, q, 0);
        b.ins().brif(more, body, &[pos1.into(), q.into()], after, &[pos1.into()]);
        b.seal_block(body);

        b.switch_to_block(after);
        b.seal_block(after);
        let dstart = b.block_params(after)[0];
        // prepend '-' at dstart-1 when negative (write unconditionally, adopt iff neg).
        let signpos = b.ins().iadd_imm(dstart, -1);
        let minus = b.ins().iconst(I8, '-' as i64);
        let saddr = b.ins().iadd(scratch_base, signpos);
        b.ins().store(MemFlags::trusted(), minus, saddr, Offset32::new(0));
        let start = b.ins().select(is_neg, signpos, dstart);
        let len = b.ins().isub(end, start);
        starts.push(start);
        lens.push(len);
    }

    // ── total byte length, object size (size = padded_bytes + HDR + 16 = HDR + nf*8,
    //    since nf = cells + 2 for the blen/clen + java-hash cells). Derived from HDR,
    //    NOT the literal 40 it used to be: under `shrink-header` HDR is 16 and a stale
    //    literal would over-size every inline-concat string by 8 bytes. ──
    let mut total = b.ins().iconst(I64, lit_total);
    for len in &lens {
        total = b.ins().iadd(total, *len);
    }
    let t7 = b.ins().iadd_imm(total, 7);
    let padded = b.ins().band_imm(t7, -8);
    let cells = b.ins().ushr_imm(padded, 3);
    let nf = b.ins().iadd_imm(cells, 2);
    let size = b.ins().iadd_imm(padded, crate::nextgen::HDR as i64 + 16);

    // ── TLAB bump test. Spill roots first so the slow (callback) path can GC. ──
    spill_for_gc(b, vars, stack, locals_ptr);
    let fast = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64); // result cell

    let cur = b.use_var(cursor_var);
    let next = b.ins().load(I64, MemFlags::trusted(), cur, Offset32::new(0));
    let limit = b.ins().load(I64, MemFlags::trusted(), cur, Offset32::new(8));
    let nn = b.ins().iadd(next, size);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, nn, limit);
    b.ins().brif(fits, fast, &[], slow, &[]);

    // ── fast: publish cursor, lay header, shred bytes, fold hash, no GC. ──
    b.switch_to_block(fast);
    b.seal_block(fast);
    b.ins().store(MemFlags::trusted(), nn, cur, Offset32::new(0));
    let nf_sh = b.ins().ishl_imm(nf, 32);
    let hw = b.ins().bor_imm(nf_sh, cid);
    b.ins().store(MemFlags::trusted(), hw, next, Offset32::new(0)); // class|nfields
    let zero64 = b.ins().iconst(I64, 0);
    b.ins().store(MemFlags::trusted(), zero64, next, Offset32::new(8)); // fwd (+hash when 16B)
    if crate::nextgen::HDR > 16 {
        b.ins().store(MemFlags::trusted(), zero64, next, Offset32::new(16)); // identity hash
    }
    let tot32 = b.ins().ireduce(I32, total);
    b.ins().store(MemFlags::trusted(), tot32, next, Offset32::new(STR_JIT_BLEN_OFF));
    b.ins().store(MemFlags::trusted(), tot32, next, Offset32::new(STR_JIT_CLEN_OFF));
    let dstb = b.ins().iadd_imm(next, STR_JIT_BYTES_OFF as i64);

    // walk segments, advancing a running write offset `woff` and — when fused — the
    // running Java hash `hacc`, which is the number the second pass used to recompute.
    let mut woff = b.ins().iconst(I64, 0);
    // `None` when the switch is off, so the legacy arm emits not one extra instruction.
    let mut hacc: Option<Value> = if fuse { Some(b.ins().iconst(I32, 0)) } else { None };
    let mut ai = 0usize;
    for seg in &shape.segs {
        match seg {
            ConcatSeg::Lit(bytes) => {
                let base = b.ins().iadd(dstb, woff);
                if let Some(h) = hacc {
                    emit_store_lit_chunked(b, base, bytes);
                    // h = h*31^k + C, both baked. Folds away entirely when this is the
                    // leading segment (h is still the constant 0).
                    let (pow, sum) = lit_hash_coeffs(bytes);
                    let hm = b.ins().imul_imm(h, pow as i64);
                    hacc = Some(b.ins().iadd_imm(hm, sum as i64));
                } else {
                    for (k, &byte) in bytes.iter().enumerate() {
                        let cb = b.ins().iconst(I8, byte as i64);
                        b.ins().store(MemFlags::trusted(), cb, base, Offset32::new(k as i32));
                    }
                }
                woff = b.ins().iadd_imm(woff, bytes.len() as i64);
            }
            ConcatSeg::IntArg(_) => {
                let src = b.ins().iadd(scratch_base, starts[ai]);
                let dst = b.ins().iadd(dstb, woff);
                let len = lens[ai];
                // byte-copy loop: for i in 0..len { dst[i] = src[i] } — and, when
                // fused, `h = h*31 + src[i]` off the byte just loaded, so the digits
                // are written once and read once.
                let cloop = b.create_block();
                b.append_block_param(cloop, I64); // i
                if fuse {
                    b.append_block_param(cloop, I32); // running hash
                }
                let cbody = b.create_block();
                let cdone = b.create_block();
                if fuse {
                    b.append_block_param(cdone, I32); // hash on exit
                }
                let zi = b.ins().iconst(I64, 0);
                match hacc {
                    Some(h) => {
                        b.ins().jump(cloop, &[zi.into(), h.into()]);
                    }
                    None => {
                        b.ins().jump(cloop, &[zi.into()]);
                    }
                }
                b.switch_to_block(cloop);
                let i = b.block_params(cloop)[0];
                let hin = if fuse { Some(b.block_params(cloop)[1]) } else { None };
                let go = b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                match hin {
                    Some(h) => b.ins().brif(go, cbody, &[], cdone, &[h.into()]),
                    None => b.ins().brif(go, cbody, &[], cdone, &[]),
                };
                b.switch_to_block(cbody);
                b.seal_block(cbody);
                let sa = b.ins().iadd(src, i);
                let da = b.ins().iadd(dst, i);
                let byte = b.ins().load(I8, MemFlags::trusted(), sa, Offset32::new(0));
                b.ins().store(MemFlags::trusted(), byte, da, Offset32::new(0));
                let i1 = b.ins().iadd_imm(i, 1);
                match hin {
                    Some(h) => {
                        // digits and '-' are ASCII, so byte == char: the same fold
                        // `java_string_hash` performs over this segment.
                        let bc = b.ins().uextend(I32, byte);
                        let h31 = b.ins().imul_imm(h, 31);
                        let hn = b.ins().iadd(h31, bc);
                        b.ins().jump(cloop, &[i1.into(), hn.into()]);
                    }
                    None => {
                        b.ins().jump(cloop, &[i1.into()]);
                    }
                }
                b.seal_block(cloop);
                b.switch_to_block(cdone);
                b.seal_block(cdone);
                if fuse {
                    hacc = Some(b.block_params(cdone)[0]);
                }
                woff = b.ins().iadd(woff, len);
                ai += 1;
            }
        }
    }

    // Java hashCode: folded above when `fuse`, otherwise the original second pass
    // over [0..total): h = h*31 + (byte as char).
    let hfin = if let Some(h) = hacc {
        h
    } else {
        let hloop = b.create_block();
        b.append_block_param(hloop, I64); // i
        b.append_block_param(hloop, I32); // h
        let hbody = b.create_block();
        let hdone = b.create_block();
        b.append_block_param(hdone, I32); // final h
        let z0 = b.ins().iconst(I64, 0);
        let h0 = b.ins().iconst(I32, 0);
        b.ins().jump(hloop, &[z0.into(), h0.into()]);
        b.switch_to_block(hloop);
        let hi = b.block_params(hloop)[0];
        let hh = b.block_params(hloop)[1];
        let hgo = b.ins().icmp(IntCC::UnsignedLessThan, hi, total);
        b.ins().brif(hgo, hbody, &[], hdone, &[hh.into()]);
        b.switch_to_block(hbody);
        b.seal_block(hbody);
        let ba = b.ins().iadd(dstb, hi);
        let bb = b.ins().load(I8, MemFlags::trusted(), ba, Offset32::new(0));
        let bc32 = b.ins().uextend(I32, bb);
        let h31 = b.ins().imul_imm(hh, 31);
        let hn = b.ins().iadd(h31, bc32);
        let hi1 = b.ins().iadd_imm(hi, 1);
        b.ins().jump(hloop, &[hi1.into(), hn.into()]);
        b.seal_block(hloop);
        b.switch_to_block(hdone);
        b.seal_block(hdone);
        b.block_params(hdone)[0]
    };
    b.ins().store(MemFlags::trusted(), hfin, next, Offset32::new(STR_JIT_HASH_OFF));
    b.ins().jump(merge, &[next.into()]);

    // ── slow: nursery couldn't fit → the callback assembles + collects + retries. ──
    b.switch_to_block(slow);
    b.seal_block(slow);
    for (i, a) in args.iter().enumerate() {
        let cell = to_cell(b, *a);
        b.ins().stack_store(cell, env.args_slot, (i as i32) * 8);
    }
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let cls = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, idx as i64);
    let nv = b.ins().iconst(I64, n as i64);
    let call = b.ins().call(env.indy, &[cls, cpv, buf, nv]);
    let res = b.inst_results(call)[0];
    b.ins().jump(merge, &[res.into()]);

    // ── merge: reload roots (forwarded on the slow path) + push the result. ──
    b.switch_to_block(merge);
    b.seal_block(merge);
    let result = b.block_params(merge)[0];
    reload_after_gc(b, vars, stack, locals_ptr);
    push(stack, result, JT::Ref);
}

// ═══════════════ `String.equals` / `String.isEmpty`, emitted ═══════════════

/// ⭐**KILL SWITCH `RAVA_NO_FAST_STRINGS3=1`** — DEFAULT ON, and the block-wide
/// `RAVA_NO_EMIT_MORE` (via `crate::flags::emit_more_disabled`) turns it off with
/// everything else in the round.
///
/// Read at EMIT time only. With it set, `emit_str_ops3` claims nothing and the two
/// sites emit exactly the dispatch they emitted before — a `String.equals` site goes
/// back through the PIC / `rava_invoke` / `natives::str_dispatch` chain and a
/// `String.isEmpty` site with it — so a build with the switch set is a true
/// one-binary A/B against a build without it.
///
/// Costs if removed: every `String.equals` in compiled code leaves machine code for a
/// host call that ends in `as_string_strict` (which materialises the argument's text)
/// to answer a question that is, for the overwhelming majority of calls, the single
/// `cmp` of two interned pointers.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fast_strings3_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_STRINGS3").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Sites CLAIMED at emit time, by shape. Host-side and incremented once per compiled
/// site — deliberately NOT a counter in the emitted code: a single relaxed atomic on a
/// hot emitted path measured **-15%** in this tree (it is not the `lock add`, it is that
/// the optimiser may then not hoist anything across it), so the fast arms below contain
/// no bookkeeping at all.
#[cfg(feature = "next-gen-gc")]
pub(crate) static S3_EQUALS_SITES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static S3_ISEMPTY_SITES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// The operand-stack slots still holding a REFERENCE when a diamond's cold arm may
/// collect. They cross the merge as block parameters — the fast arms hand their own
/// (unmoved) values straight through, the cold arm hands back what the collector
/// forwarded. Without this the emitter would have to REFUSE any site with a live ref on
/// the stack, which in real bytecode is nearly all of them (the `ArrayList.size` arm
/// fired ZERO times a boot for exactly that reason before it was given this treatment).
#[cfg(feature = "next-gen-gc")]
fn live_ref_slots(stack: &[SV]) -> Vec<usize> {
    stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect()
}

/// The cold arm shared by both shapes: the ORDINARY `invokevirtual` dispatch this site
/// would have emitted — `env.invoke` (`rava_invoke`) with kind 1 and the same constant
/// pool index — so a refused guard is only slower and never different.
///
/// ⚠**`env.invoke`, NOT `env.fu_invoke`.** The fastutil callback routes on (name,
/// descriptor) before it knows the receiver's class, and it already serves an
/// `isEmpty()Z`; handing it a String receiver under that key is a question it was never
/// asked. `rava_invoke` is the unmodified general dispatch and is the exact oracle.
///
/// Returns the packed result cell and the merge arguments. Spills every live root first
/// (the callee can run arbitrary guest code and collect), reloads the locals through
/// `def_var` — SSA-correct on one arm of a diamond — and returns the FORWARDED ref-stack
/// values read back out of the spill area, which is what the merge must carry (passing
/// the pre-call SSA values would hand the merge pointers the collector has moved).
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
fn emit_str_cold_invoke(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &[SV],
    vars: &[Variable],
    locals_ptr: Value,
    cp: u16,
    argc: usize,
    ref_slots: &[usize],
) -> Vec<cranelift::codegen::ir::BlockArg> {
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, 1); // 1 = invokevirtual, the opcode this arm replaced
    let nv = b.ins().iconst(I64, argc as i64);
    let call = b.ins().call(env.invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in ref_slots {
        // Ref stack values live in the spill area ABOVE max_locals — see `spill_for_gc`.
        let fwd = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        args.push(fwd.into());
    }
    args
}

/// STRING INTRINSIC (next-gen-gc): claim `String.equals(Object)` and `String.isEmpty()`
/// at an `invokevirtual` site and emit them as loads and compares. Returns `Some(())`
/// when the site was claimed (result already pushed), `None` when it was not — the
/// caller then emits exactly what it emitted before, so this is purely additive.
///
/// ⛔**NOTHING IS CLAIMED BEFORE IT CAN BE FINISHED.** The arity and operand-kind checks
/// happen before the first `pop`, so this never returns `None` having already consumed
/// operands — a half-claimed site would leave the abstract stack desynchronised from the
/// emitted IR for the whole rest of the method.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_str_ops3(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    cp: u16,
    name: &str,
    desc: &str,
) -> Option<()> {
    if !fast_strings3_enabled() {
        return None;
    }
    // ⚠**BOTH OPERANDS MUST ALREADY BE MODELLED AS REFERENCES.** The emitted code compares
    // the operand cells as I64 pointers; an `SV` the abstract stack types as `Int` holds an
    // I32 SSA value, and mixing the two is a Cranelift type error at COMPILE time, not a
    // wrong answer at run time. The descriptor says what the operands are, but this asserts
    // what the emitter's own model says they are, which is the thing being read here.
    // Read as plain booleans, not through a closure: a closure capturing `stack` would keep
    // an immutable borrow alive across the match arms, which then take it mutably.
    let n = stack.len();
    let top_ref = n >= 1 && stack[n - 1].t == JT::Ref;
    let second_ref = n >= 2 && stack[n - 2].t == JT::Ref;
    match (name, desc) {
        ("equals", "(Ljava/lang/Object;)Z") if top_ref && second_ref => {
            S3_EQUALS_SITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            emit_str_equals(b, env, stack, vars, locals_ptr, cp)
        }
        ("isEmpty", "()Z") if top_ref => {
            S3_ISEMPTY_SITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            emit_str_isempty(b, env, stack, vars, locals_ptr, cp)
        }
        _ => None,
    }
}

/// ⭐**`String.isEmpty()` IS ONE LOAD AND ONE COMPARE.**
///
/// **THE GUARD PROVES**: the receiver is a non-null arena pointer (`emit_is_arena`:
/// non-zero with no tag in bits 40..43) whose header class word IS the ArenaString class
/// — so `[recv + STR_JIT_CLEN_OFF]` is this string's cached CHAR length, the very number
/// `length()` returns. `isEmpty()` is specified as `length() == 0`, so the answer is that
/// field against zero and nothing else is read.
///
/// ⚠**THE CLASS-ID LOAD IS LOAD-BEARING, NOT INHERITED CAUTION.** A raw low-address cell
/// is not necessarily an arena object: under `RAVA_PINNED_PTRS`, `abi::box_ref` flows a
/// legacy `Value::Ref(h)` as its PINNED MIRROR ADDRESS, which is `< 2^40` and passes
/// `emit_is_arena` exactly like an arena pointer. The neighbouring `length()`/`hashCode()`
/// arm infers "arena receiver at a `java/lang/String` site ⇒ ArenaString" from the verifier
/// alone and has no such protection. This arm proves its own precondition instead, for one
/// 4-byte load off the header line the `clen` load is about to touch anyway.
///
/// COLD ARM: the ordinary `invokevirtual` dispatch (`emit_str_cold_invoke`) — a
/// `Value::Str` handle receiver, and a null receiver, which must raise the same
/// NullPointerException the uncompiled site raises. Answering a null receiver here would
/// be a silent semantic change, so `emit_is_arena` rejecting zero is load-bearing.
#[cfg(feature = "next-gen-gc")]
fn emit_str_isempty(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    cp: u16,
) -> Option<()> {
    use crate::nextgen::STR_JIT_CLEN_OFF;
    let recv_sv = stack.pop()?;
    let recv = recv_sv.v;
    let ref_slots = live_ref_slots(&stack[..]);

    let cls_blk = b.create_block();
    let fast = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64); // packed boolean result cell
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }
    let extra: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();
    let zero64 = b.ins().iconst(I64, 0);
    let one64 = b.ins().iconst(I64, 1);
    let scid = b.ins().iconst(I32, crate::nextgen::string_class_id() as i64);

    // Guard 1: a non-null arena pointer.
    let is_a = emit_is_arena(b, recv);
    b.ins().brif(is_a, cls_blk, &[], slow, &[]);

    // Guard 2: its header class word really is the ArenaString class.
    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    let cid = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(0));
    let is_str = b.ins().icmp(IntCC::Equal, cid, scid);
    b.ins().brif(is_str, fast, &[], slow, &[]);

    // Fast: the cached CHAR length against zero. No call, no allocation, no GC.
    b.switch_to_block(fast);
    b.seal_block(fast);
    let clen = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(STR_JIT_CLEN_OFF));
    let empty = b.ins().icmp_imm(IntCC::Equal, clen, 0);
    let cell = b.ins().select(empty, one64, zero64);
    let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![cell.into()];
    fast_args.extend_from_slice(&extra);
    b.ins().jump(merge, &fast_args);

    // Cold: the unchanged dispatch — boxed receiver, or null (which must throw).
    b.switch_to_block(slow);
    b.seal_block(slow);
    let recv_cell = to_cell(b, recv_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    let slow_args = emit_str_cold_invoke(b, env, &stack[..], vars, locals_ptr, cp, 1, &ref_slots);
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Int);
    stack.push(sv);
    Some(())
}

/// ⭐**`String.equals(Object)` WITHOUT LEAVING MACHINE CODE — IDENTITY FIRST.**
///
/// Compiled code answered this by leaving the machine code entirely: PIC miss →
/// `rava_invoke` → `natives::str_dispatch`'s `"equals"` arm → `as_string_strict`, i.e. a
/// host call and an argument render, to decide a question that for interned literals is
/// one `cmp` of two pointers. String constants ARE interned in this engine
/// (`vm::strings::ldc_intern` hands back ONE `RStr` per literal for the process, and the
/// arena string pool does the same on its side), which is why the identity test is not a
/// micro-optimisation here: it is the answer for the common `key.equals(SOME_CONSTANT)`
/// shape, and it reads no memory at all.
///
/// **THE GUARDS, IN ORDER, AND WHAT EACH PROVES:**
///   1. `recv != 0 && recv == other` — the two operand cells are bit-equal and non-null,
///      hence the same object, hence equal. The non-null conjunct is load-bearing, not
///      defensive: `null == null` must NOT be answered `true` here, it must reach the
///      cold arm so a null receiver throws exactly as it did before.
///   2. `emit_is_arena(recv)` — a non-null arena pointer. A boxed `Value::Str` handle
///      carries a tag in bits 40..43 and fails this, so it goes cold.
///   3. `other == 0` — `x.equals(null)` is specified FALSE (never an NPE), answered
///      inline with no load.
///   4. `emit_is_arena(other)` — a tagged non-null argument could still be a
///      `Value::Str` that COMPARES EQUAL, so it must go cold; only an arena pointer is
///      decidable here.
///   5. `other`'s header class word == ArenaString.
///   6. `recv`'s class word == ArenaString.
///
/// ⚠**GUARDS 5 AND 6 ARE NOT BELT-AND-BRACES, AND A NON-STRING IS NOT ANSWERED `false`
/// HERE.** The obvious shortcut — "the argument is an arena object of another class, so a
/// TYPED `equals` is decidedly false" — is UNSOUND in this engine, twice over. A raw
/// low-address cell is not necessarily an arena object: under `RAVA_PINNED_PTRS`,
/// `abi::box_ref` flows a legacy `Value::Ref(h)` as its PINNED MIRROR ADDRESS, which is
/// `< 2^40` and therefore passes `emit_is_arena` exactly like an arena pointer. And a
/// legacy handle can BE a string as far as `equals` is concerned: `as_string_strict` — the
/// very function `str_dispatch`'s `"equals"` arm calls — accepts a `Value::Ref` carrying a
/// `$str` synthetic field. So a cell that is arena-SHAPED but not ArenaString-CLASSED goes
/// to the cold arm, which is the only thing that can tell those apart. The class-word test
/// is what makes the whole shape safe under the pinned-pointer flag; the neighbouring
/// `length()`/`hashCode()` arm, which infers "arena receiver ⇒ ArenaString" from the
/// verifier alone, has no such protection.
///   7. the 8-byte word at `STR_JIT_BLEN_OFF` — which is `blen` and `clen` side by side
///      — equal for both, or FALSE. That is the specified length compare AND the byte
///      length in ONE load and ONE compare per operand.
///   8. the bodies, eight bytes at a time.
///
/// **WHY BYTES DECIDE CHARS.** Both operands are ArenaStrings, whose text IS their inline
/// UTF-8; UTF-8 is an injective encoding, so identical bytes are identical char sequences
/// and different bytes at equal length are different ones. The mask is what makes the
/// word loop sound: inline bytes start at `STR_BYTES_OFF` (`HDR + 16`, and `HDR` is a
/// multiple of 8, so the base is 8-aligned) and are allocated in whole 8-byte cells
/// (`nf = 2 + ceil(blen/8)`), so every word this loop reads lies inside BOTH objects' own
/// storage — but the trailing PAD bytes past `blen` are never initialised and may differ
/// between two equal strings. `(a ^ b) & mask`, with `mask = (1 << 8*rem) - 1` on the
/// final partial word, compares only the bytes that exist. `blen == 0` exits the loop at
/// once with `true`, which is what two distinct empty strings must answer.
///
/// ⛔**NO HASH PRE-TEST, DELIBERATELY.** A cached-hash compare would reject unequal
/// same-length strings in O(1) — but it costs two more loads on EVERY call including the
/// equal ones, where it proves nothing and the body loop still has to run; and unequal
/// strings almost always differ inside their first eight bytes, which the first loop trip
/// already finds. Two loads bought on the certain path to save a trip on the uncertain one
/// is the memo trade this tree has measured going the wrong way twice.
///
/// COLD ARM: `emit_str_cold_invoke` — the ordinary `invokevirtual` dispatch, unchanged,
/// which is the oracle for a null receiver (throws), a boxed `Value::Str` on either side,
/// and any receiver shape the class-word guard refuses.
#[cfg(feature = "next-gen-gc")]
fn emit_str_equals(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    vars: &[Variable],
    locals_ptr: Value,
    cp: u16,
) -> Option<()> {
    use crate::nextgen::{STR_JIT_BLEN_OFF, STR_JIT_BYTES_OFF};
    let other_sv = stack.pop()?;
    let recv_sv = stack.pop()?;
    let (recv, other) = (recv_sv.v, other_sv.v);
    let ref_slots = live_ref_slots(&stack[..]);

    let k_recv = b.create_block(); // not the same object → test the receiver's kind
    let k_null = b.create_block(); // argument null?
    let k_oarena = b.create_block(); // argument an arena pointer?
    let k_ocls = b.create_block(); // argument an ArenaString?
    let k_rcls = b.create_block(); // receiver an ArenaString?
    let k_len = b.create_block(); // lengths equal?
    let wloop = b.create_block();
    b.append_block_param(wloop, I64); // byte index i, always a multiple of 8
    let wbody = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64); // packed boolean result cell
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }
    // The live references the fast arms hand straight through, unmoved: no arm below that
    // reaches the merge without the callback can allocate, so none can have been forwarded.
    let extra: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();
    // Constants minted in the entry block, which dominates every block below.
    let zero64 = b.ins().iconst(I64, 0);
    let one64 = b.ins().iconst(I64, 1);
    let scid = b.ins().iconst(I32, crate::nextgen::string_class_id() as i64);
    let mut yes: Vec<cranelift::codegen::ir::BlockArg> = Vec::with_capacity(1 + extra.len());
    let mut no: Vec<cranelift::codegen::ir::BlockArg> = Vec::with_capacity(1 + extra.len());
    yes.push(one64.into());
    yes.extend_from_slice(&extra);
    no.push(zero64.into());
    no.extend_from_slice(&extra);

    // 1. identity: the same non-null cell → true, no memory touched.
    let nz = b.ins().icmp(IntCC::NotEqual, recv, zero64);
    let same = b.ins().icmp(IntCC::Equal, recv, other);
    let ident = b.ins().band(nz, same);
    b.ins().brif(ident, merge, &yes, k_recv, &[]);

    // 2. receiver must be a non-null arena pointer.
    b.switch_to_block(k_recv);
    b.seal_block(k_recv);
    let ra = emit_is_arena(b, recv);
    b.ins().brif(ra, k_null, &[], slow, &[]);

    // 3. `x.equals(null)` is FALSE, and is not a fault.
    b.switch_to_block(k_null);
    b.seal_block(k_null);
    let onull = b.ins().icmp(IntCC::Equal, other, zero64);
    b.ins().brif(onull, merge, &no, k_oarena, &[]);

    // 4. a tagged (handle/box/array) argument may still be a string → cold.
    b.switch_to_block(k_oarena);
    b.seal_block(k_oarena);
    let oa = emit_is_arena(b, other);
    b.ins().brif(oa, k_ocls, &[], slow, &[]);

    // 5. the argument's header class word must BE the ArenaString class. Anything else
    //    goes COLD, not false: a low-address cell can be a pinned mirror of a legacy
    //    handle, and a legacy handle carrying `$str` compares equal to a string.
    b.switch_to_block(k_ocls);
    b.seal_block(k_ocls);
    let ocid = b.ins().load(I32, MemFlags::trusted(), other, Offset32::new(0));
    let o_is_str = b.ins().icmp(IntCC::Equal, ocid, scid);
    b.ins().brif(o_is_str, k_rcls, &[], slow, &[]);

    // 6. and the receiver proves its own class rather than inheriting the assumption.
    b.switch_to_block(k_rcls);
    b.seal_block(k_rcls);
    let rcid = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(0));
    let r_is_str = b.ins().icmp(IntCC::Equal, rcid, scid);
    b.ins().brif(r_is_str, k_len, &[], slow, &[]);

    // 7. lengths: ONE i64 load per operand covers `blen` and `clen` together (they are
    //    adjacent i32s at `STR_JIT_BLEN_OFF`, which is 8-aligned because HDR is).
    b.switch_to_block(k_len);
    b.seal_block(k_len);
    let rword = b.ins().load(I64, MemFlags::trusted(), recv, Offset32::new(STR_JIT_BLEN_OFF));
    let oword = b.ins().load(I64, MemFlags::trusted(), other, Offset32::new(STR_JIT_BLEN_OFF));
    let len_eq = b.ins().icmp(IntCC::Equal, rword, oword);
    // Little-endian: the low half of that word is `blen`, so there is no second load.
    let blen32 = b.ins().ireduce(I32, rword);
    let blen64 = b.ins().uextend(I64, blen32);
    let rbase = b.ins().iadd_imm(recv, STR_JIT_BYTES_OFF as i64);
    let obase = b.ins().iadd_imm(other, STR_JIT_BYTES_OFF as i64);
    b.ins().brif(len_eq, wloop, &[zero64.into()], merge, &no);

    // 8. wloop(i): consumed every byte → true; else compare the next word.
    b.switch_to_block(wloop);
    let i = b.block_params(wloop)[0];
    let done = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, blen64);
    b.ins().brif(done, merge, &yes, wbody, &[]);

    // one 8-byte compare, masked down to the bytes that are really inside the strings.
    b.switch_to_block(wbody);
    b.seal_block(wbody);
    let raddr = b.ins().iadd(rbase, i);
    let oaddr = b.ins().iadd(obase, i);
    let rw = b.ins().load(I64, MemFlags::trusted(), raddr, Offset32::new(0));
    let ow = b.ins().load(I64, MemFlags::trusted(), oaddr, Offset32::new(0));
    let diff = b.ins().bxor(rw, ow);
    let rem = b.ins().isub(blen64, i); // >= 1 here, since i < blen
    let full = b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, rem, 8);
    // 8..56 on the partial arm (rem is 1..7 there); the value is dead when `full`.
    let shamt = b.ins().imul_imm(rem, 8);
    let bit = b.ins().ishl(one64, shamt);
    let partial = b.ins().iadd_imm(bit, -1);
    let allones = b.ins().iconst(I64, -1);
    let mask = b.ins().select(full, allones, partial);
    let masked = b.ins().band(diff, mask);
    let eq = b.ins().icmp_imm(IntCC::Equal, masked, 0);
    let inext = b.ins().iadd_imm(i, 8);
    b.ins().brif(eq, wloop, &[inext.into()], merge, &no);
    b.seal_block(wloop); // both preds (k_len, wbody) now emitted

    // Cold: the unchanged dispatch. All four preds (k_recv, k_oarena, k_ocls, k_rcls)
    // are emitted above, so the block can be sealed here.
    b.switch_to_block(slow);
    b.seal_block(slow);
    let recv_cell = to_cell(b, recv_sv);
    let other_cell = to_cell(b, other_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    b.ins().stack_store(other_cell, env.args_slot, 8);
    let slow_args = emit_str_cold_invoke(b, env, &stack[..], vars, locals_ptr, cp, 2, &ref_slots);
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Int);
    stack.push(sv);
    Some(())
}
