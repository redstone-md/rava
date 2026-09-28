// Element-independent map: out[i] = f(in[i]) — the M2 auto-parallel target. `map` is its own
// whole-method loop (like the reduce benches) so the whole-method parallel analyzer can match:
// no accumulator, no cross-iteration dependency, each iteration writes only its own out[i].
// Pure long body → workers stay alloc-free / GC-safe. run() folds the output for a bit-exact gate.
public class MapLoop {
    static void map(long[] out, long[] in, int n) {
        // in[i] inlined (no scratch temp) so the store target `aload out; iload i` opens the
        // loop body — the whole-method map analyzer's canonical shape.
        for (int i = 0; i < n; i++) {
            out[i] = in[i] * in[i] + (in[i] >>> 13) - (in[i] << 7);
        }
    }

    static long run(int n) {
        long[] in = new long[n];
        long[] out = new long[n];
        for (int i = 0; i < n; i++) in[i] = (long) i * 2654435761L + 1;
        map(out, in, n);
        long s = 0;
        for (int i = 0; i < n; i++) s += out[i];
        return s;
    }

    public static void main(String[] a) {
        for (int w = 0; w < 10; w++) run(1_000_000);          // warmup
        long t0 = System.nanoTime();
        long s = run(20_000_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
