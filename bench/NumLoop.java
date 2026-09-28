// M3 T2 step 3b target: a counted loop with a call-free, branchless, alloc-free, primitive-array
// body and a LOCAL bound — the minimal shape the counted-loop fork parallelises. Each iteration
// writes a disjoint out[i], so workers run disjoint index sub-ranges. Deterministic SUM confirms
// the parallel counted-loop fork is bit-exact vs the JVM.
public class NumLoop {
    static void fill(long[] out, long[] a, int n) {
        for (int i = 0; i < n; i++) {
            long v = a[i];
            out[i] = v * v + (long) i;
        }
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 1000000;
        long[] a = new long[n];
        long[] out = new long[n];
        for (int i = 0; i < n; i++) {
            a[i] = i * 2654435761L + 12345L;
        }
        long best = Long.MAX_VALUE;
        long sum = 0;
        for (int rep = 0; rep < 8; rep++) {
            long t0 = System.nanoTime();
            fill(out, a, n);
            long dt = System.nanoTime() - t0;
            if (dt < best) best = dt;
            sum = 0;
            for (int i = 0; i < n; i++) {
                sum += out[i];
            }
        }
        System.out.println("HOTNS=" + best + " SUM=" + sum + " N=" + n);
    }
}
