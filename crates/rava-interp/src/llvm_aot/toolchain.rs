//! Finding and driving the external toolchain, and linking the object it produces.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::llvm_aot::*;

/// Link the `.text` + `.rdata` sections of a clang COFF object into ONE contiguous blob
/// (entry at offset 0) and patch `.text`'s RIP-relative relocations so its instructions
/// reach the constants now appended after it. This is what unblocks AVX SIMD: clang's loop
/// vectoriser spills vector constants (shuffle masks, broadcasts) into `.rdata` referenced
/// `lea/vbroadcast (%rip)` — with both sections in one blob the RIP displacement is
/// BASE-INDEPENDENT (`target_blob_off − (field_blob_off + 4 + N)`), so we resolve it here,
/// once, and it stays valid wherever `alloc_exec` maps the blob (RWX). No relocs (scalar /
/// reloc-free double path) → returns `.text` alone, byte-identical to the old extraction.
/// `None` (→ Cranelift) on anything we can't bind: an external/undefined symbol, a target
/// section we didn't lay out, or a non-RIP relocation type (absolute addrs need a runtime
/// base we don't have at blob-link time).
///
/// COFF layout: 20-byte header, `num_sections`×40-byte section headers, then per-section
/// raw data / 10-byte relocations; an 18-byte symbol table at `PointerToSymbolTable`.
pub(crate) fn coff_link_text_rdata(obj: &[u8]) -> Option<Vec<u8>> {
    let u16_at = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(obj.get(o..o + 2)?.try_into().ok()?)) };
    let u32_at = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(obj.get(o..o + 4)?.try_into().ok()?)) };
    let num_sections = u16_at(2)? as usize;
    let sym_ptr = u32_at(8)? as usize;
    let opt_header = u16_at(16)? as usize;

    // Pass 1: read every section header; lay .text (first) then each .rdata/.rodata into the
    // blob (honouring each section's declared alignment — AVX aligned loads fault otherwise).
    // `sec_off[section_number]` (1-based) = the section's start offset in the blob, or None.
    struct Sec { raw_ptr: usize, raw_size: usize, reloc_ptr: usize, num_relocs: usize, is_text: bool, rdata: bool, align: usize }
    let mut secs: Vec<Sec> = Vec::with_capacity(num_sections);
    let mut so = 20 + opt_header;
    for _ in 0..num_sections {
        let name = obj.get(so..so + 8)?;
        let raw_size = u32_at(so + 16)? as usize;
        let raw_ptr = u32_at(so + 20)? as usize;
        let reloc_ptr = u32_at(so + 24)? as usize;
        let num_relocs = u16_at(so + 32)? as usize;
        let chars = u32_at(so + 36)?;
        let av = ((chars >> 20) & 0xF) as u32; // IMAGE_SCN_ALIGN: 2^(av-1) bytes
        let align = if av >= 1 { 1usize << (av - 1) } else { 1 };
        let is_text = name.starts_with(b".text\0") || name == b".text\0\0\0";
        let rdata = name.starts_with(b".rdata") || name.starts_with(b".rodata");
        secs.push(Sec { raw_ptr, raw_size, reloc_ptr, num_relocs, is_text, rdata, align });
        so += 40;
    }

    let mut blob: Vec<u8> = Vec::new();
    let mut sec_off: Vec<Option<usize>> = vec![None; num_sections + 1]; // 1-based
    // .text first — entry point at blob offset 0.
    let text_i = secs.iter().position(|s| s.is_text)?;
    {
        let t = &secs[text_i];
        blob.extend_from_slice(obj.get(t.raw_ptr..t.raw_ptr + t.raw_size)?);
        sec_off[text_i + 1] = Some(0);
    }
    // .rdata/.rodata sections after, each aligned (min 32B — safe for AVX-256 constants).
    for (i, s) in secs.iter().enumerate() {
        if !s.rdata || s.raw_size == 0 {
            continue;
        }
        let align = s.align.max(32);
        while blob.len() % align != 0 {
            blob.push(0);
        }
        sec_off[i + 1] = Some(blob.len());
        blob.extend_from_slice(obj.get(s.raw_ptr..s.raw_ptr + s.raw_size)?);
    }

    // Pass 2: patch .text's relocations. Every unresolved reloc → bail (correctness).
    let text = &secs[text_i];
    let text_off = 0usize;
    for r in 0..text.num_relocs {
        let ro = text.reloc_ptr + r * 10;
        let field_va = u32_at(ro)? as usize; // offset within .text
        let sym_idx = u32_at(ro + 4)? as usize;
        let rtype = u16_at(ro + 8)?;
        // REL32 (4) .. REL32_5 (9): RIP-relative, reference point = field+4+N, N=type-4.
        let n = match rtype {
            4..=9 => (rtype - 4) as usize,
            _ => return None, // absolute / GOT / other — needs a runtime base we lack
        };
        // Symbol → (section_number, value). 18-byte entries; SectionNumber @12 (1-based,
        // 0 = external/undefined → bail), Value @8.
        let se = sym_ptr + sym_idx * 18;
        let sym_val = u32_at(se + 8)? as i64;
        let sym_sec = i16::from_le_bytes(obj.get(se + 12..se + 14)?.try_into().ok()?);
        if sym_sec <= 0 {
            return None; // undefined / absolute symbol — cannot bind
        }
        let tgt_sec_off = (*sec_off.get(sym_sec as usize)?)?; // target section not laid out → bail
        let field_off = text_off + field_va;
        // COFF addend is the pre-existing value IN THE FIELD — read it from the copied .text
        // (blob[0..text_size]), NOT from the raw COFF file at `field_off` (that offset is
        // .text-relative; the file offset would be text.raw_ptr + field_va).
        let addend = i32::from_le_bytes(blob.get(field_off..field_off + 4)?.try_into().ok()?) as i64;
        let target = tgt_sec_off as i64 + sym_val + addend;
        let disp = target - (field_off as i64 + 4 + n as i64);
        if std::env::var_os("RAVA_LLVM_LINK_DBG").is_some() {
            eprintln!("[LINK] reloc field=0x{field_off:x} type={rtype} sym_sec={sym_sec} sym_val={sym_val} addend={addend} tgt_sec_off=0x{tgt_sec_off:x} target=0x{target:x} disp={disp} blob_len=0x{:x}", blob.len());
        }
        let d = i32::try_from(disp).ok()?; // must fit RIP-relative i32
        blob.get_mut(field_off..field_off + 4)?.copy_from_slice(&d.to_le_bytes());
    }
    Some(blob)
}

/// ELIGIBILITY PROBE (RAVA_LLVM_PROBE, read-only): classify a hot method's bytecode to
/// measure whether adding getfield/putfield to the subset would unlock it —
///   0 = ALREADY fully in the LLVM subset (compiled today),
///   1 = would be fully in-subset IF getfield(0xb4)/putfield(0xb5) were added (the only
///       out-of-subset ops are field access),
///   2 = BLOCKED by some OTHER op (invoke/new/checkcast/…) — fields won't help.
/// Tallied + printed periodically so the verdict survives a culled boot. Zero codegen
/// impact (never changes what compiles).
#[cfg(feature = "next-gen-gc")]
pub fn llvm_probe(bc: &[u8]) {
    use std::sync::atomic::{AtomicU64, Ordering};
    // Categorise a hot method by its FIRST out-of-subset opcode (the op that bails it). invokestatic
    // (0xb8) + arrays are NOW in-subset, so IN = structurally LLVM-eligible today (upper bound — the
    // whole-tree driver still bail-checks each invokestatic callee's eligibility, which the probe
    // can't resolve without the loader). The rest split by WHAT blocks them.
    static IN: AtomicU64 = AtomicU64::new(0); // fully in-subset (LLVM flies, modulo callee check)
    static INV: AtomicU64 = AtomicU64::new(0); // dynamic invoke: virtual/special/interface/indy
    static REF: AtomicU64 = AtomicU64::new(0); // object/reference ops (fields, new, aaload, casts…)
    static OTH: AtomicU64 = AtomicU64::new(0); // other (switch/monitor/static/float/wide…)
    let mut first_blocker: Option<u8> = None;
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if !llvm_subset_op(op) {
            first_blocker = Some(op);
            break;
        }
        match crate::jit::insn_len_at(bc, pc) {
            Some(l) => pc += l,
            None => {
                first_blocker = Some(0xff); // undecodable → OTHER
                break;
            }
        }
    }
    let bucket = match first_blocker {
        None => &IN,
        Some(0xb6 | 0xb7 | 0xb9 | 0xba) => &INV, // invokevirtual/special/interface/dynamic
        // reference/object handling: getfield/putfield (object fields), aaload/aastore, new/
        // anewarray/newarray, checkcast/instanceof, aconst_null, areturn, athrow, ifnull/ifnonnull.
        Some(0x01 | 0xb0 | 0xb4 | 0xb5 | 0x32 | 0x53 | 0xbb | 0xbc | 0xbd | 0xc0 | 0xc1 | 0xc6 | 0xc7 | 0xbf) => &REF,
        Some(_) => &OTH,
    };
    let tot = bucket.fetch_add(1, Ordering::Relaxed) + 1;
    let sum = IN.load(Ordering::Relaxed) + INV.load(Ordering::Relaxed) + REF.load(Ordering::Relaxed) + OTH.load(Ordering::Relaxed);
    if tot % 50 == 0 || sum % 200 == 0 {
        eprintln!(
            "[LLVM-PROBE] hot methods: in-subset={} blocked-by-dyn-invoke={} blocked-by-ref/object={} blocked-other={}",
            IN.load(Ordering::Relaxed), INV.load(Ordering::Relaxed), REF.load(Ordering::Relaxed), OTH.load(Ordering::Relaxed)
        );
    }
}

/// Compile LLVM IR text → COFF object via `clang -O2 -c` → the `.text` machine code.
/// `None` on any failure (no clang, IR error, no `.text`) so the caller falls back to
/// Cranelift/interp. Uses a content-hashed temp path so concurrent/repeat compiles don't
/// collide and a warm OS file cache helps.
pub fn clang_compile_ir(ir: &str) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir();
    let key = crate::cnp::aot_key(ir.as_bytes());
    let ll = dir.join(format!("rava_llvm_{key}.ll"));
    let obj = dir.join(format!("rava_llvm_{key}.o"));
    std::fs::write(&ll, ir).ok()?;
    // -ffp-contract=off: forbid fusing fmul+fadd into an FMA (one rounding) — the JVM does
    // two separate IEEE roundings, and a chaotic double recurrence (e.g. ArithLoop's
    // d=d*c+x over millions of iters) diverges wildly otherwise. Keeps bit-exactness.
    // NO -march by default: an AVX target (`-march=native`) makes clang's loop vectoriser
    // spill VECTOR CONSTANTS into `.rdata` and reference them RIP-relative from `.text` —
    // but `coff_dot_text` extracts `.text` ONLY, so those relocs dangle (verified: 7
    // .text→.rdata relocs at -march=native vs 0 at generic → SIMD code ran on garbage
    // constants: slow + only accidentally bit-exact). Reloc-free is the LLVM tier's core
    // invariant (same reason doubles ride %consts, not inline literals). Generic x86-64
    // stays reloc-free (scalar, BCE+unroll via !invariant.load). SIMD is unblocked only by
    // teaching the loader to bind .rdata relocs (a follow-up). RAVA_LLVM_MARCH forces a CPU
    // for that experiment.
    let mut cmd = std::process::Command::new(clang_exe());
    cmd.args(["-O2", "-ffp-contract=off"]);
    if let Some(march) = llvm_march(ir) {
        cmd.arg(format!("-march={march}"));
    }
    let status = cmd
        .args(["-c", "-target", "x86_64-w64-windows-gnu", "-o"])
        .arg(&obj)
        .arg(&ll)
        .output()
        .ok()?;
    let text = if status.status.success() {
        std::fs::read(&obj).ok().and_then(|o| coff_link_text_rdata(&o))
    } else {
        None
    };
    let _ = std::fs::remove_file(&ll);
    let _ = std::fs::remove_file(&obj);
    text
}
