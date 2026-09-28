//! Build-time LLVM-AOT tier. Emit LLVM IR text for a bytecode method, compile it with the
//! `clang` subprocess (`-O2`) to a COFF (Windows x64) object, extract the `.text` machine
//! code, and hand it to `cnp::CompiledObj` — the SAME relocatable-code container the
//! Cranelift AOT uses. This buys near-C2 codegen quality WITHOUT linking LLVM into the
//! runtime: `clang` is a build-time tool only (single-binary preserved), and a populated
//! AOT disk cache is loaded at runtime with no toolchain present. Cranelift stays the
//! runtime fallback for cache-misses / mods / machines without clang.
//!
//! Covers the pure int/long/double numeric subset — position-independent `.text`, no
//! relocations. Double (and the d2i/d2l saturate threshold) constants ride the runtime
//! `%consts` array instead of inline literals, so clang never emits a `.rdata` constant pool
//! (which would need a RIP-relative relocation our `.text`-only extraction can't bind);
//! `coff_dot_text` refuses any `.text` that still carries a relocation, so a stray one falls
//! back to Cranelift rather than running garbage. Increment 2 will parse the COFF relocation
//! table for callback-carrying methods (calls to rava_* + the const table), mirroring the
//! Cranelift Abs8 reloc path. Wired into aot_run under RAVA_LLVM_AOT; a couple of helpers are
//! test-only, hence:



#![allow(dead_code)]

/// The `clang` executable: `RAVA_CLANG` override, else `clang` on PATH.
fn clang_exe() -> String {
    std::env::var("RAVA_CLANG").unwrap_or_else(|_| "clang".to_string())
}

/// The `-march` for this IR, or `None` for the generic reloc-free baseline. SIMD default =
/// `x86-64-v3` (AVX2) for INTEGER/long/array IR: integer SIMD is bit-identical and AVX2
/// avoids AVX-512's frequency downclock (ArrReduce n=10M: AVX2 4ms vs native/AVX-512 40ms vs
/// Cranelift 16ms). Float/double IR stays generic — AVX FP diverges from the JVM's scalar
/// rounding (the double recurrence) even with -ffp-contract=off. RAVA_LLVM_MARCH overrides
/// (=native for AVX-512, =x86-64 to disable SIMD on a pre-AVX2 CPU). Shared by the compiler
/// AND the disk-cache key so a cached blob is never mismatched to a different ISA.
fn llvm_march(ir: &str) -> Option<String> {
    if ir.contains("double") || ir.contains("float") {
        return None;
    }
    Some(std::env::var("RAVA_LLVM_MARCH").unwrap_or_else(|_| "x86-64-v3".to_string()))
}

/// Compile the IR through the PERSISTENT blob cache (opt-in `RAVA_AOT_DIR`). The clang
/// `.text`+`.rdata` blob is a pure function of `(bytecode, march-ISA)` — the constant VALUES
/// ride the runtime `%consts` array (rebuilt free by `emit_llvm_ir`), not the machine code —
/// so the cache key is `SHA256(bytecode)` + the march tag, and only the blob is persisted
/// (the cvec is never cached). A hit reads the blob and skips the ~150-500ms clang subprocess
/// entirely; `CompiledObj::from_code(blob).link()` then binds it (relocs are already patched
/// into the blob by coff_link_text_rdata). Wrong-ISA safety: an AVX2 blob is keyed `x86-64-v3`
/// (a portable ISA LEVEL, valid on any v3 CPU), distinct from generic / native keys.
pub fn clang_compile_ir_cached(ir: &str) -> Option<Vec<u8>> {
    let path = crate::cnp::aot_dir().map(|d| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        // Key on the IR — it fully + uniquely determines the blob (a whole-tree module's IR
        // encodes every function, so two roots with identical bytecode but different callees
        // don't collide). The march tag distinguishes AVX2/generic ISA.
        h.update(ir.as_bytes());
        let digest = h.finalize();
        let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        let march = llvm_march(ir).unwrap_or_else(|| "generic".to_string());
        // LLVM_CACHE_FORMAT bumps if the blob layout / linker changes (invalidates stale blobs).
        d.join(format!("llvm1_{march}_{hex}.text"))
    });
    if let Some(p) = &path {
        if let Ok(blob) = std::fs::read(p) {
            if !blob.is_empty() {
                return Some(blob);
            }
        }
    }
    let text = clang_compile_ir(ir)?;
    if let Some(p) = &path {
        let _ = std::fs::write(p, &text);
    }
    Some(text)
}

/// Basic-block leaders for the int-numeric subset (pc 0, every s16 branch target, and the
/// fall-through after each conditional). Sorted + deduped.
fn leaders(bc: &[u8]) -> Option<Vec<usize>> {
    let mut set = vec![0usize];
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let len = crate::jit::insn_len_at(bc, pc)?;
        if matches!(op, 0x99..=0xa4 | 0xa7) {
            let target = (pc as i32 + ((bc[pc + 1] as i16) << 8 | bc[pc + 2] as i16) as i32) as usize;
            set.push(target);
            if op != 0xa7 {
                set.push(pc + len);
            }
        }
        pc += len;
    }
    set.retain(|&p| p < bc.len());
    set.sort_unstable();
    set.dedup();
    Some(set)
}

/// The int+long numeric subset the LLVM emitter handles (reloc-free, no callbacks).
/// A superset of `aot_clif::supported` adding the long opcodes (lload/lstore/ladd/…/lreturn,
/// lcmp, i2l/l2i) — covers pure long/int folds like bench/ArithLoop's xorshift kernel.
pub(crate) fn llvm_subset_op(op: u8) -> bool {
    matches!(op,
        // int
        0x02..=0x08 | 0x10 | 0x11 | 0x15 | 0x1a..=0x1d | 0x36 | 0x3b..=0x3e
        | 0x57 | 0x59 | 0x60 | 0x64 | 0x68 | 0x74 | 0x78 | 0x7a | 0x7c
        | 0x7e | 0x80 | 0x82 | 0x84 | 0x99..=0xa4 | 0xa7 | 0xac
        // long
        | 0x09 | 0x0a | 0x16 | 0x1e..=0x21 | 0x37 | 0x3f..=0x42
        | 0x61 | 0x65 | 0x69 | 0x75 | 0x79 | 0x7b | 0x7d | 0x7f | 0x81 | 0x83
        | 0x94 | 0x85 | 0x88 | 0xad
        // numeric constant pool loads (ldc/ldc_w int, ldc2_w long/double)
        | 0x12 | 0x13 | 0x14
        // double (serial AOT — bit-exact, not associative-parallel)
        | 0x0e | 0x0f | 0x18 | 0x26..=0x29 | 0x39 | 0x47..=0x4a
        | 0x63 | 0x67 | 0x6b | 0x6f | 0x77 | 0x97 | 0x98
        | 0x87 | 0x8a | 0x8e | 0x8f | 0xaf
        // primitive-array access: aload (ref local = array handle), arraylength,
        // iaload/laload (read), iastore/lastore (write). Address + IR bounds-check +
        // direct load/store — clang -O2 does BCE + auto-vectorises (the SIMD win).
        // A ref reaching ONLY these ops IS an array (any escape op — astore/areturn/
        // invoke/getfield — is out-of-subset → the whole method bails to Cranelift).
        | 0x19 | 0x2a..=0x2d | 0xbe | 0x2e | 0x2f | 0x4f | 0x50
        // invokestatic (structurally allowed; the emitter bails unless the callee is ALSO
        // LLVM-eligible + all-numeric-static, resolved via the whole-tree driver → emitted as
        // a same-module @fN + a direct internal call clang inlines).
        | 0xb8
    )
}

/// Does this method touch the ARRAY-DESCRIPTOR table (iaload/laload/iastore/lastore/
/// arraylength)? Gates the two reserved `consts` slots (0 = the desc-base cell pointer,
/// 1 = the fault-fn pointer). Kept identical in `emit_llvm_ir` (reserve) and
/// `aot_compile_link` (fill) so the indices line up. `aload` alone doesn't need the
/// descriptor (it just loads the handle cell), so it is excluded here.
pub(crate) fn llvm_uses_arrays(bc: &[u8]) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        if matches!(bc[pc], 0x2e | 0x2f | 0x4f | 0x50 | 0xbe) {
            return true;
        }
        match crate::jit::insn_len_at(bc, pc) {
            Some(l) => pc += l,
            None => return false,
        }
    }
    false
}

/// Cheap structural check: is every opcode in `bc` inside the LLVM numeric subset? This is
/// exactly `emit_llvm_fn`'s early-bail loop, without the O(IR) codegen — a fast armability proxy
/// (an out-of-subset op = a getfield/ref/nested-invoke that would bail the fast-path body).
pub(crate) fn llvm_bytecode_in_subset(bc: &[u8]) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        if !llvm_subset_op(bc[pc]) {
            return false;
        }
        match crate::jit::insn_len_at(bc, pc) {
            Some(l) => pc += l,
            None => return false,
        }
    }
    true
}

/// IR TEMPLATE — array descriptor load for access site `s`. Null-check (→ indirect
/// `rava_array_fault` via `consts[1]`, then `ret i64 0`), then read `(data, len)` from
/// `ArrayDesc` (16-byte `#[repr(C)]{data:i64@0, len:i32@8}`) indexed by the handle's low
/// 32 bits into the live descriptor base (`consts[0]` = &jit_ctx → `[+0]` = the current
/// `array_descs` base, so a realloc is seen). Leaves the current block = `annn{s}` and
/// returns (data_i64_ssa, len_i32_ssa).
///
/// ```llvm
///   %anull{s} = icmp eq i64 {arr}, 0
///   br i1 %anull{s}, label %aflt{s}, label %annn{s}
/// aflt{s}:                         ; null → NPE
///   %aff{s} = inttoptr i64 (load consts[1]) to ptr
///   call i64 %aff{s}(i64 {arr}, i64 0)
///   ret i64 0
/// annn{s}:                         ; array non-null
///   %abase{s} = load i64, [ (inttoptr load consts[0]) ]      ; array_descs base
///   %adpp{s}  = inttoptr (%abase{s} + ((arr & 0xFFFFFFFF) << 4)) to ptr
///   %adata{s} = load i64,  %adpp{s}           ; ArrayDesc.data
///   %alen{s}  = load i32, (%adpp{s} + 8)       ; ArrayDesc.len
/// ```
fn emit_arr_desc(out: &mut String, s: usize, arr: &str) -> (String, String) {
    out.push_str(&format!("  %anull{s} = icmp eq i64 {arr}, 0\n"));
    out.push_str(&format!("  br i1 %anull{s}, label %aflt{s}, label %annn{s}\n"));
    out.push_str(&format!("aflt{s}:\n"));
    out.push_str(&format!("  %affp{s} = getelementptr i64, ptr %consts, i64 1\n"));
    out.push_str(&format!("  %affb{s} = load i64, ptr %affp{s}\n"));
    out.push_str(&format!("  %aff{s} = inttoptr i64 %affb{s} to ptr\n"));
    out.push_str(&format!("  %afr{s} = call i64 %aff{s}(i64 {arr}, i64 0)\n"));
    out.push_str("  ret i64 0\n");
    // `!invariant.load !0`: the descriptor base/data/len do NOT change during THIS call
    // (the LLVM subset has no allocation → no put_array → no realloc), so clang may hoist
    // + CSE them. That unifies the arraylength `len` with the per-access `len` → proves
    // `i < len` → ELIMINATES the bounds branch → the fault block dies → the loop becomes
    // countable and auto-vectorises. (Across calls the load re-executes, seeing the current
    // value — invariance is only within one execution, which holds here.)
    out.push_str(&format!("annn{s}:\n"));
    out.push_str(&format!("  %abcp{s} = getelementptr i64, ptr %consts, i64 0\n"));
    out.push_str(&format!("  %abcell{s} = load i64, ptr %abcp{s}, !invariant.load !0\n"));
    out.push_str(&format!("  %abcellp{s} = inttoptr i64 %abcell{s} to ptr\n"));
    out.push_str(&format!("  %abase{s} = load i64, ptr %abcellp{s}, !invariant.load !0\n"));
    out.push_str(&format!("  %ah{s} = and i64 {arr}, 4294967295\n"));
    out.push_str(&format!("  %aoff{s} = shl i64 %ah{s}, 4\n"));
    out.push_str(&format!("  %adp{s} = add i64 %abase{s}, %aoff{s}\n"));
    out.push_str(&format!("  %adpp{s} = inttoptr i64 %adp{s} to ptr\n"));
    out.push_str(&format!("  %adata{s} = load i64, ptr %adpp{s}, !invariant.load !0\n"));
    out.push_str(&format!("  %alenp{s} = getelementptr i8, ptr %adpp{s}, i64 8\n"));
    out.push_str(&format!("  %alen{s} = load i32, ptr %alenp{s}, !invariant.load !0\n"));
    (format!("%adata{s}"), format!("%alen{s}"))
}

/// IR TEMPLATE — element address for site `s` after `emit_arr_desc`. UNSIGNED bounds
/// check `idx >=u len` (folds `idx<0` → huge → OOB) → indirect fault + `ret`, else the
/// address `data + (idx << shift)`. The `icmp`/`br` are IR-visible so clang -O2 can
/// prove them redundant (`i<len`, `len==a.length`) and ELIMINATE the check, then
/// auto-vectorise. Leaves the current block = `aok{s}`; returns the element-ptr ssa.
fn emit_arr_addr(
    out: &mut String, s: usize, arr: &str, idx: &str, data: &str, len: &str, shift: u32,
) -> String {
    out.push_str(&format!("  %aub{s} = icmp uge i32 {idx}, {len}\n"));
    out.push_str(&format!("  br i1 %aub{s}, label %aoflt{s}, label %aok{s}\n"));
    out.push_str(&format!("aoflt{s}:\n"));
    out.push_str(&format!("  %aofp{s} = getelementptr i64, ptr %consts, i64 1\n"));
    out.push_str(&format!("  %aofb{s} = load i64, ptr %aofp{s}\n"));
    out.push_str(&format!("  %aof{s} = inttoptr i64 %aofb{s} to ptr\n"));
    out.push_str(&format!("  %aoi{s} = sext i32 {idx} to i64\n"));
    out.push_str(&format!("  %aor{s} = call i64 %aof{s}(i64 {arr}, i64 %aoi{s})\n"));
    out.push_str("  ret i64 0\n");
    out.push_str(&format!("aok{s}:\n"));
    out.push_str(&format!("  %aidx{s} = zext i32 {idx} to i64\n"));
    out.push_str(&format!("  %aeoff{s} = shl i64 %aidx{s}, {shift}\n"));
    out.push_str(&format!("  %aea{s} = add i64 {data}, %aeoff{s}\n"));
    out.push_str(&format!("  %aeap{s} = inttoptr i64 %aea{s} to ptr\n"));
    format!("%aeap{s}")
}

/// Emit LLVM IR text for a pure int/long-numeric method (the reloc-free subset above).
/// Locals are `alloca i64` (holds an int's sign-extended low-32 OR a long) so clang's
/// mem2reg lifts loop-carried locals to SSA registers — the LICM/scalar-evolution win
/// Cranelift misses with locals-in-memory. The operand stack tracks `(ssa_value, is_long)`:
/// int values are i32, long values i64. `None` (→ Cranelift/interp fallback) on any op
/// outside the subset or a non-empty stack across a branch. Signature = the AOT ABI
/// `i64 f(ptr locals, ptr consts)` (an int result sign-extends into the i64 cell).
/// A resolved LLVM-eligible invokestatic target: its in-module function name (`f1`, `f2`, …),
/// the callee's `max_locals` (for the argument buffer), and its descriptor (for arg marshalling +
/// return decode). Keyed by the caller's constant-pool index in a per-function `callee_names` map.
#[derive(Clone)]
pub struct LlvmCallee {
    pub name: String,
    pub max_locals: u32,
    pub desc: String,
}

/// Parse a method descriptor into (arg kinds, return kind), collapsed to the numeric subset:
/// `I` (int/bool/byte/char/short), `J` (long), `D` (double); return may also be `V`. Any other
/// kind (float, reference, array) → `None` (the callee isn't numeric-int/long/double-eligible).
pub fn llvm_desc_args_ret(desc: &str) -> Option<(Vec<char>, char)> {
    let b = desc.as_bytes();
    let mut i = desc.find('(')? + 1;
    let mut args = Vec::new();
    while i < b.len() && b[i] != b')' {
        match b[i] {
            b'I' | b'Z' | b'B' | b'C' | b'S' => args.push('I'),
            b'J' => args.push('J'),
            b'D' => args.push('D'),
            _ => return None, // float / ref / array — outside the numeric subset
        }
        i += 1;
    }
    let ret = match *b.get(i + 1)? {
        b'I' | b'Z' | b'B' | b'C' | b'S' => 'I',
        b'J' => 'J',
        b'D' => 'D',
        b'V' => 'V',
        _ => return None,
    };
    Some((args, ret))
}

/// Emit a SINGLE-function LLVM module (the existing scalar/array path). Reserves the module-wide
/// array-ABI consts ([0]=desc-base cell ptr, [1]=fault-fn ptr — filled at link) + the `!0`
/// invariant-load metadata around the one function `@f`. The whole-tree driver
/// (aot_compile_link, inc 6.3) will build MULTI-function modules directly via `emit_llvm_fn`;
/// this wrapper keeps the unit tests + the non-invoke fast path byte-identical.
pub fn emit_llvm_ir(
    bc: &[u8],
    max_locals: u32,
    ldc_of: &dyn Fn(u16) -> Option<crate::aot_clif::LdcConst>,
) -> Option<(String, Vec<i64>)> {
    let mut out = String::new();
    let mut consts: Vec<i64> = Vec::new();
    let uses_arrays = llvm_uses_arrays(bc);
    if uses_arrays {
        consts.push(0); // [0] desc-base cell ptr
        consts.push(0); // [1] fault-fn ptr
    }
    let no_callees = std::collections::HashMap::new();
    emit_llvm_fn(&mut out, &mut consts, "f", false, bc, max_locals, &no_callees, ldc_of)?;
    if uses_arrays {
        // Metadata node for the array descriptor loads' `!invariant.load` (canonical empty
        // "invariant" marker) — module-level, emitted once after the function(s).
        out.push_str("!0 = !{}\n");
    }
    Some((out, consts))
}

/// Compile a pure int-numeric method to relocatable machine code via the LLVM tier, or
/// `None` (→ Cranelift/interp) if it's outside the subset or clang is unavailable/fails.
pub fn compile_numeric_llvm(
    bc: &[u8],
    max_locals: u32,
    ldc_of: &dyn Fn(u16) -> Option<crate::aot_clif::LdcConst>,
) -> Option<(Vec<u8>, Vec<i64>)> {
    let (ir, consts) = emit_llvm_ir(bc, max_locals, ldc_of)?;
    Some((clang_compile_ir(&ir)?, consts))
}

/// Is a `clang` toolchain available (so the LLVM-AOT tier can run)? Cheap-ish (spawns
/// `clang --version` once); callers should cache the result.
pub fn clang_available() -> bool {
    std::process::Command::new(clang_exe())
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
mod emit;
pub(crate) use emit::*;
mod toolchain;
pub(crate) use toolchain::*;
mod tests;
pub(crate) use tests::*;
