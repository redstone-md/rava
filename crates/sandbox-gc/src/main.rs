//! SANDBOX LAB — bump-arena + generational copying nursery GC + raw-pointer
//! objects. ISOLATED: rava never imports this. Goal: empirically prove the
//! generational/raw-pointer model brings the alloc-churn microbenchmarks
//! (Allocs / StringOps) to ~1.0-1.5x of the JVM, vs rava's current handle +
//! Box<Object> + Vec + mark-sweep model.
//!
//! Three allocators compared per workload:
//!   - BUMP+GEN  : this prototype (Cheney nursery copy → tenured, raw ptrs).
//!   - BOX-BASE  : models rava-current (Box<Object{Vec}> per alloc, no reuse —
//!                 Allocs never triggers rava's GC, so every `new` mallocs).
//!   - JVM ref   : the measured `java -cp out Allocs` HOTNS (~50ms) for scale.
//!
//! RAVA_HARD_MB caps the tenured region; overflow PANICS (sandbox-safe, never
//! freezes the host).



mod heap;
use heap::*;
mod benches;
use benches::*;
use std::time::Instant;

// ─────────────────────────── object model (raw) ───────────────────────────
// Arena object layout (repr C, 8-byte aligned):
//   [0]  class_id : u32
//   [4]  nfields  : u32
//   [8]  fwd      : *mut u8   (GC forwarding ptr; null = not yet copied)
//   [16] cells    : [i64; nfields]   (primitive value OR raw *mut u8 for refs)
const HDR: usize = 16;
#[inline]
fn obj_size(nf: u32) -> usize {
    HDR + nf as usize * 8
}
#[inline]
unsafe fn o_class(p: *mut u8) -> u32 {
    *(p as *mut u32)
}
#[inline]
unsafe fn o_nf(p: *mut u8) -> u32 {
    *(p.add(4) as *mut u32)
}
#[inline]
unsafe fn o_fwd(p: *mut u8) -> *mut u8 {
    *(p.add(8) as *mut *mut u8)
}
#[inline]
unsafe fn o_set_fwd(p: *mut u8, f: *mut u8) {
    *(p.add(8) as *mut *mut u8) = f;
}
#[inline]
unsafe fn cell(p: *mut u8, i: usize) -> i64 {
    *(p.add(HDR + i * 8) as *mut i64)
}
#[inline]
unsafe fn set_cell(p: *mut u8, i: usize, v: i64) {
    *(p.add(HDR + i * 8) as *mut i64) = v;
}

struct ClassDesc {
    nfields: u32,
    ref_mask: u32, // bit i set ⇒ cell i is a raw object pointer (trace + forward)
}

// ─────────────────────────── regions ───────────────────────────
// Fixed-capacity boxed slice → stable base pointer (never reallocs; the GC
// collects instead of growing, so raw pointers into it stay valid).
struct Region {
    buf: Box<[u8]>,
    top: usize,
}
impl Region {
    fn new(cap: usize) -> Self {
        Region {
            buf: vec![0u8; cap].into_boxed_slice(),
            top: 0,
        }
    }
    #[inline]
    fn base(&self) -> *mut u8 {
        self.buf.as_ptr() as *mut u8
    }
    #[inline]
    fn cap(&self) -> usize {
        self.buf.len()
    }
    #[inline]
    fn contains(&self, p: *mut u8) -> bool {
        let b = self.base() as usize;
        let a = p as usize;
        a >= b && a < b + self.buf.len()
    }
}

const POINT: u32 = 0;

struct BObj {
    cells: Vec<i64>,
}
const STR: u32 = 0;
fn java_hashcode(b: &[u8]) -> i32 {
    let mut h: i32 = 0;
    for &c in b {
        h = h.wrapping_mul(31).wrapping_add(c as i32);
    }
    h
}
fn build_item(i: i32, buf: &mut [u8]) -> usize {
    // "item" + (i&1023) + "-" + (i%7)
    let mut n = 0;
    for &c in b"item" {
        buf[n] = c;
        n += 1;
    }
    n += write_int(i & 1023, &mut buf[n..]);
    buf[n] = b'-';
    n += 1;
    n += write_int(i % 7, &mut buf[n..]);
    n
}
fn hard_mb() -> usize {
    std::env::var("RAVA_HARD_MB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096)
}

fn main() {
    let cap_mb = hard_mb();
    println!("== sandbox-gc lab (RAVA_HARD_MB={cap_mb}) ==");
    let old_cap = (cap_mb * 1024 * 1024) / 2; // tenured cap; overflow panics
    let young_cap = 32 * 1024 * 1024; // 32 MiB nursery

    // ---------- Allocs (8M) ----------
    let iters = 8_000_000;
    let t = Instant::now();
    let (sum_g, gcs, copied) = allocs_bumpgen(iters, young_cap, old_cap);
    let ms_g = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let sum_b = allocs_boxbase(iters);
    let ms_b = t.elapsed().as_secs_f64() * 1e3;
    println!("\n[Allocs 8M]  expect SUM=191999972465152");
    println!(
        "  BUMP+GEN : {ms_g:8.1} ms   SUM={sum_g}  ({gcs} minor GCs, {copied} objs tenured)"
    );
    println!("  BOX-BASE : {ms_b:8.1} ms   SUM={sum_b}");
    println!("  speedup  : {:.1}x  (BOX/BUMP)", ms_b / ms_g);
    println!("  vs JVM   : ~{:.2}x  (JVM Allocs ≈ 50 ms)", ms_g / 50.0);
    assert_eq!(sum_g, sum_b, "BUMP vs BOX SUM mismatch");

    // ---------- StringOps (2M) ----------
    let iters = 2_000_000;
    let t = Instant::now();
    let s_g = strings_bumpgen(iters, young_cap, old_cap);
    let sms_g = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let s_b = strings_boxbase(iters);
    let sms_b = t.elapsed().as_secs_f64() * 1e3;
    println!("\n[StringOps 2M]");
    println!("  BUMP+GEN : {sms_g:8.1} ms   ACC={s_g}");
    println!("  BOX-BASE : {sms_b:8.1} ms   ACC={s_b}");
    println!("  speedup  : {:.1}x  (BOX/BUMP)", sms_b / sms_g);
    println!("  vs JVM   : ~{:.2}x  (JVM StringOps ≈ 25 ms)", sms_g / 25.0);
    assert_eq!(s_g, s_b, "string BUMP vs BOX ACC mismatch");

    // ---------- TIER-1: TLAB multi-thread (overtake via cores) ----------
    println!("\n[TLAB multi-thread Allocs — 8M total, JVM single-thread ≈ 51 ms]");
    for &n in &[1usize, 2, 4, 8, 16] {
        // each thread gets a small private nursery + tenured slice (its TLAB)
        let yc = 8 * 1024 * 1024;
        let oc = old_cap / n.max(1);
        let (ms, _sum) = parallel_allocs(8_000_000, n, yc, oc);
        let speedup_vs_jvm = 51.0 / ms;
        let tag = if speedup_vs_jvm >= 1.0 {
            format!("{speedup_vs_jvm:.2}x FASTER than JVM")
        } else {
            format!("{:.2}x JVM", 1.0 / speedup_vs_jvm)
        };
        println!("  {n:2} threads : {ms:7.1} ms   ({tag})");
    }

    // ---------- TIER-2: SIMD bulk-zeroing ----------
    let (gibs, _) = simd_bulk_zero_bench();
    println!("\n[SIMD bulk-zero TLAB]  {gibs:.1} GiB/s  (AVX bulk stores → zeroing never the bottleneck)");

    // ---------- TIER-3: escape analysis / scalar replacement ----------
    let (me, ms, ae, as_) = scalar_replacement_demo(8_000_000, young_cap, old_cap);
    assert_eq!(ae, as_, "scalar-replace SUM mismatch");
    println!("\n[Escape analysis → scalar replacement — 8M non-escaping temps]");
    println!("  ESCAPING (alloc) : {me:7.1} ms");
    println!("  SCALAR-REPLACED  : {ms:7.1} ms   ({:.1}x — alloc eliminated, 0 GC pressure)", me / ms);
    println!("\n== lab done ==");
}
