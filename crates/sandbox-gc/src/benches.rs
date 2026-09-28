//! The allocation and string benchmarks.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::*;

// ─────────────────────────── Allocs workload ───────────────────────────
// Mirrors bench/Allocs.java exactly so SUM must match (191999972465152 @ 8M).
pub(crate) fn allocs_bumpgen(iters: i32, young_cap: usize, old_cap: usize) -> (i64, u64, u64) {
    let classes = vec![ClassDesc {
        nfields: 4,            // x,y,z,next
        ref_mask: 1 << 3,      // cell 3 (next) is a ref
    }];
    let mut h = GenHeap::new(young_cap, old_cap, classes);
    let mut acc: i64 = 0;
    // roots[0] == head (the live chain root). Kept in the roots array so a minor
    // GC during alloc rewrites it to the tenured copy.
    let mut roots: [*mut u8; 1] = [std::ptr::null_mut()];
    for i in 0..iters {
        let p = h.alloc(POINT, &mut roots);
        let x = i;
        let y = i.wrapping_mul(2);
        let z = i.wrapping_mul(3);
        unsafe {
            set_cell(p, 0, x as i64);
            set_cell(p, 1, y as i64);
            set_cell(p, 2, z as i64);
            set_cell(p, 3, roots[0] as i64); // p.next = head
        }
        if i & 15 == 0 {
            roots[0] = p; // head = p
        }
        acc += x.wrapping_add(y).wrapping_add(z) as i64;
        if i & 1023 == 0 {
            roots[0] = std::ptr::null_mut(); // head = null
        }
    }
    // Walk the surviving chain.
    let mut p = roots[0];
    while !p.is_null() {
        let x = unsafe { cell(p, 0) } as i32;
        acc ^= x as i64;
        p = unsafe { cell(p, 3) } as usize as *mut u8;
    }
    (acc, h.minor_gcs, h.copied_objs)
}

// BOX baseline: models rava-current (Box<Object{Vec}> per alloc, handle index,
// NO reuse — Allocs never trips rava's GC, so every `new` mallocs a Box+Vec).
pub(crate) fn allocs_boxbase(iters: i32) -> i64 {
    let mut heap: Vec<BObj> = Vec::new();
    let mut acc: i64 = 0;
    let mut head: i64 = -1; // handle (-1 = null)
    for i in 0..iters {
        let h = heap.len() as i64;
        heap.push(BObj {
            cells: vec![0i64; 4],
        });
        let x = i;
        let y = i.wrapping_mul(2);
        let z = i.wrapping_mul(3);
        let o = &mut heap[h as usize];
        o.cells[0] = x as i64;
        o.cells[1] = y as i64;
        o.cells[2] = z as i64;
        o.cells[3] = head;
        if i & 15 == 0 {
            head = h;
        }
        acc += x.wrapping_add(y).wrapping_add(z) as i64;
        if i & 1023 == 0 {
            head = -1;
        }
    }
    let mut p = head;
    while p >= 0 {
        let o = &heap[p as usize];
        acc ^= (o.cells[0] as i32) as i64;
        p = o.cells[3];
    }
    acc
}

// ─────────────────────────── StringOps workload ───────────────────────────
// Strings as arena objects: class STR, cell0=len, cells hold packed bytes (we
// store the bytes after the header, reusing the cell area as a byte buffer).
// Each iter builds "item{i&1023}-{i%7}", hashes (Java String.hashCode), most die
// immediately (only the current string is reachable → nursery reclaims them).
pub(crate) fn write_int(mut v: i32, buf: &mut [u8]) -> usize {
    if v == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 12];
    let mut k = 0;
    let neg = v < 0;
    let mut uv = (v as i64).unsigned_abs();
    let _ = &mut v;
    while uv > 0 {
        tmp[k] = b'0' + (uv % 10) as u8;
        uv /= 10;
        k += 1;
    }
    let mut n = 0;
    if neg {
        buf[0] = b'-';
        n = 1;
    }
    for j in (0..k).rev() {
        buf[n] = tmp[j];
        n += 1;
    }
    n
}

pub(crate) fn strings_bumpgen(iters: i32, young_cap: usize, old_cap: usize) -> i64 {
    // STR object: nfields packs a byte buffer (max ~16 bytes → 2 cells) + len.
    // ref_mask 0 (no object refs). We bump a header + the bytes inline.
    let classes = vec![ClassDesc {
        nfields: 4, // up to 32 bytes inline
        ref_mask: 0,
    }];
    let mut h = GenHeap::new(young_cap, old_cap, classes);
    let mut roots: [*mut u8; 1] = [std::ptr::null_mut()];
    let mut acc: i64 = 0;
    let mut scratch = [0u8; 32];
    for i in 0..iters {
        let p = h.alloc(STR, &mut roots);
        let len = build_item(i, &mut scratch);
        unsafe {
            // write len into a side: store len in cell 0's low byte region by
            // writing bytes starting at the cell area; we keep len separately via
            // the bytes themselves. Copy bytes into the inline cell buffer.
            let dst = p.add(HDR);
            std::ptr::copy_nonoverlapping(scratch.as_ptr(), dst, len);
            roots[0] = p; // keep the current string briefly live (a real root)
        }
        let bytes = &scratch[..len];
        acc += java_hashcode(bytes) as i64;
        acc ^= len as i64;
        if bytes.starts_with(b"item0") {
            acc += 3;
        }
        roots[0] = std::ptr::null_mut(); // drop → dies young
    }
    acc
}

pub(crate) fn strings_boxbase(iters: i32) -> i64 {
    let mut heap: Vec<Vec<u8>> = Vec::new();
    let mut acc: i64 = 0;
    let mut scratch = [0u8; 32];
    for i in 0..iters {
        let len = build_item(i, &mut scratch);
        heap.push(scratch[..len].to_vec()); // malloc per string (rava-style churn)
        let bytes = &scratch[..len];
        acc += java_hashcode(bytes) as i64;
        acc ^= len as i64;
        if bytes.starts_with(b"item0") {
            acc += 3;
        }
    }
    acc
}

// ═══════════════ TIER-1: TLAB multi-thread (zero-sync bump) ═══════════════
// Each OS thread owns a PRIVATE GenHeap (its TLAB nursery) — alloc = a
// thread-local pointer bump with NO atomics, NO mutex on the hot path. N threads
// allocate fully in parallel → aggregate throughput scales with cores and
// overtakes the single-threaded JVM bench. Splits the 8M-object total across
// `nthreads`; wall time = the slowest thread (true parallel).
pub(crate) fn parallel_allocs(total_iters: i32, nthreads: usize, young_cap: usize, old_cap: usize) -> (f64, i64) {
    let per = total_iters / nthreads as i32;
    let t = Instant::now();
    let handles: Vec<_> = (0..nthreads)
        .map(|_| {
            std::thread::spawn(move || {
                // PRIVATE heap per thread = the TLAB. No shared state, no locks.
                let (sum, _, _) = allocs_bumpgen(per, young_cap, old_cap);
                sum
            })
        })
        .collect();
    let mut sum = 0i64;
    for h in handles {
        sum = sum.wrapping_add(h.join().unwrap());
    }
    (t.elapsed().as_secs_f64() * 1e3, sum)
}

// ═══════════════ TIER-2: SIMD bulk-zeroing of a fresh TLAB ═══════════════
// A recycled TLAB is zeroed in bulk before reuse. `slice::fill(0)` lowers to the
// platform's widest stores (AVX `vmovdqu` on x86-64) — multi-GB/s, so zeroing is
// never the bottleneck. Measures GiB/s over repeated wipes of a 64 MiB region.
pub(crate) fn simd_bulk_zero_bench() -> (f64, f64) {
    let mb = 64usize;
    let mut region = vec![0u8; mb * 1024 * 1024].into_boxed_slice();
    let rounds = 64;
    // touch once so pages are resident (exclude first-touch fault from timing)
    region.fill(1);
    let t = Instant::now();
    let mut acc = 0u8;
    for r in 0..rounds {
        region.fill(0); // → AVX bulk stores
        // defeat dead-store elimination: read one byte that depends on the wipe
        acc = acc.wrapping_add(region[(r * 4096) % region.len()]);
    }
    std::hint::black_box(acc);
    let s = t.elapsed().as_secs_f64();
    let gib = (mb as f64 * rounds as f64) / 1024.0;
    (gib / s, s * 1e3)
}

// ═══════════════ TIER-3: escape analysis → scalar replacement ═══════════════
// When the JIT proves an object never escapes its method, it replaces the alloc
// with the object's fields held in registers/locals — ZERO allocation, ZERO GC
// pressure. Demo: a temp "Point" used only locally each iter. ESCAPING form
// allocates it in the arena (nursery churn); SCALAR-REPLACED form is what
// Cranelift e-graph EA emits — the fields become plain locals, the object never
// exists. Same SUM, the alloc vanishes.
pub(crate) fn scalar_replacement_demo(iters: i32, young_cap: usize, old_cap: usize) -> (f64, f64, i64, i64) {
    // ESCAPING: allocate a non-escaping temp each iter (no EA).
    let classes = vec![ClassDesc { nfields: 4, ref_mask: 0 }];
    let mut h = GenHeap::new(young_cap, old_cap, classes);
    let mut roots: [*mut u8; 1] = [std::ptr::null_mut()];
    let mut acc_e: i64 = 0;
    let t = Instant::now();
    for i in 0..iters {
        let p = h.alloc(0, &mut roots);
        unsafe {
            set_cell(p, 0, i as i64);
            set_cell(p, 1, i.wrapping_mul(2) as i64);
            set_cell(p, 2, i.wrapping_mul(3) as i64);
            acc_e += cell(p, 0) + cell(p, 1) + cell(p, 2);
        }
        // p never escapes — dies immediately (not stored anywhere).
    }
    let ms_e = t.elapsed().as_secs_f64() * 1e3;

    // SCALAR-REPLACED: the object is gone; its fields are locals. black_box keeps
    // the arithmetic honest (no const-fold of the whole loop) — this is the REAL
    // cost once the allocation + field loads/stores are removed.
    let mut acc_s: i64 = 0;
    let t = Instant::now();
    for i in 0..iters {
        let x = std::hint::black_box(i) as i64;
        let y = i.wrapping_mul(2) as i64;
        let z = i.wrapping_mul(3) as i64;
        acc_s += x + y + z;
    }
    std::hint::black_box(acc_s);
    let ms_s = t.elapsed().as_secs_f64() * 1e3;
    (ms_e, ms_s, acc_e, acc_s)
}
