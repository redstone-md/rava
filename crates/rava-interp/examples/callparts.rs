//! WHAT DO THE PRIMITIVES ON AN INTERPRETED CALL ACTUALLY COST ON THIS BOX?
//!
//! Row 395 prices a static call with a FREE callee — no frame, no allocation, zero
//! arguments, served frame-free — at ~105 ns, against the JVM interpreter's 7 ns. Reading
//! the path names about twenty steps, and the temptation is to guess which one is heavy.
//! This measures the SHAPES instead: a thread-local pooled-vector take/give, an `Arc`
//! clone/drop pair, a `OnceLock` read, a small hash lookup, an atomic load. Comparing the
//! sum of the shapes against the 105 ns says whether the call is explained by its parts or
//! whether something outside the reading is dominating.
//!
//! ⚠**This is a cost MODEL, not a measurement of the product path.** It runs the same kinds
//! of operation, not the same code. It can say "twenty of these cannot add to 105 ns"; it
//! cannot say which line is hot. Numbers are min-of-N: the box carries ~29% background load.
use std::cell::RefCell;
use std::hint::black_box;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

thread_local! {
    static POOL: RefCell<Vec<Vec<u128>>> = RefCell::new((0..8).map(|_| Vec::with_capacity(8)).collect());
    static MAP: RefCell<std::collections::HashMap<(usize, u16), u32>> = RefCell::new(std::collections::HashMap::new());
}
static ATOM: AtomicUsize = AtomicUsize::new(7);
static CELL: OnceLock<bool> = OnceLock::new();

fn bench(name: &str, iters: u64, reps: usize, mut f: impl FnMut(u64) -> u64) {
    let mut best = f64::MAX;
    let mut all = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        let mut acc = 0u64;
        for i in 0..iters {
            acc = acc.wrapping_add(f(black_box(i)));
        }
        black_box(acc);
        let ns = t.elapsed().as_nanos() as f64 / iters as f64;
        all.push(ns);
        if ns < best {
            best = ns;
        }
    }
    let spread: Vec<String> = all.iter().map(|v| format!("{v:.2}")).collect();
    println!("{name:<34} {best:>6.2} ns   spread {}", spread.join("/"));
}

fn main() {
    let iters: u64 = 20_000_000;
    let reps = 5;
    CELL.get_or_init(|| false);
    let arc: Arc<Vec<u8>> = Arc::new(vec![0u8; 64]);
    MAP.with(|m| {
        let mut m = m.borrow_mut();
        for k in 0..64u16 {
            m.insert((0x1000usize, k), k as u32);
        }
    });
    let hdr = AtomicU32::new(1);

    bench("empty loop (control)", iters, reps, |i| i & 1);
    bench("OnceLock read", iters, reps, |i| (*CELL.get().unwrap() as u64) ^ (i & 1));
    bench("static atomic load (Relaxed)", iters, reps, |i| ATOM.load(Relaxed) as u64 ^ (i & 1));
    bench("AtomicU32 load (Relaxed)", iters, reps, |i| hdr.load(Relaxed) as u64 ^ (i & 1));
    bench("Arc clone + drop", iters, reps, |i| {
        let c = arc.clone();
        let n = c.len() as u64;
        drop(c);
        n ^ (i & 1)
    });
    bench("TLS pooled vec take+give", iters, reps, |i| {
        POOL.with(|p| {
            let mut v = p.borrow_mut().pop().unwrap_or_default();
            v.push(i as u128);
            let n = v.len() as u64;
            v.clear();
            p.borrow_mut().push(v);
            n ^ (i & 1)
        })
    });
    bench("TLS hashmap get", iters, reps, |i| {
        MAP.with(|m| *m.borrow().get(&(0x1000usize, (i & 63) as u16)).unwrap_or(&0) as u64)
    });
    bench("Vec push+pop (owned)", iters, reps, |i| {
        thread_local! { static V: RefCell<Vec<u128>> = RefCell::new(Vec::with_capacity(64)); }
        V.with(|v| {
            let mut v = v.borrow_mut();
            v.push(i as u128);
            v.pop().unwrap() as u64 & 1
        })
    });
}
