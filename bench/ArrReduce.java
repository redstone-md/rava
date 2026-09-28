// Compute-bound array reduction: a square-sum `sum += (long)a[i]*a[i]` over a primitive
// int[]. rava's parallel JIT (RAVA_JIT=1 RAVA_JIT_PARALLEL=1) resolves the element base
// pointer once under the GIL, fans the fold across all cores, and each worker loads
// `base[i]` directly — no per-element FFI, no bounds check (the fork proves
// `length >= limit`), GC-safe (the region allocates nothing, so the fixed-length backing
// never moves). The body is compute-bound (an lmul per element, NOT memory-bandwidth
// bound like a plain `+= a[i]`), so the fan-out beats C2's single thread. SUM is
// bit-exact. The body avoids constant-pool loads (ldc2_w), which the worker emitter does
// not replay — a long literal weight would route the loop to the serial fallback instead.
// Run via bench/arrreduce.sh.
public class ArrReduce {
    static long hot(int[] a) {
        long sum = 0;
        int n = a.length;                        // hoisted length → loop limit
        for (int i = 0; i < n; i++) sum += (long) a[i] * a[i];
        return sum;
    }
    public static void main(String[] x) {
        int n = x.length > 0 ? Integer.parseInt(x[0]) : 100_000_000;
        int[] a = new int[n];
        for (int i = 0; i < n; i++) a[i] = (i % 4096) - 2048;
        for (int w = 0; w < 8; w++) hot(a);      // warm the JIT / C2
        long t0 = System.nanoTime();
        long s = hot(a);
        System.out.println("SUM=" + s + " HOTNS=" + (System.nanoTime() - t0));
    }
}
