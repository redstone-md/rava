// M3 getstatic-worker-safety repro: a forked counted-loop body READS a static field. On a worker
// the interpreted getstatic misses the thread-local cache → getstatic_resolve → class.statics
// RefCell borrow → concurrent borrow-flag race (panic). Small N, no MC setup.
public class NumLoopStatic {
    static final long[] T = { 2, 3, 5, 7, 11, 13, 17, 19 };

    static void fill(long[] out, long[] a, int n) {
        for (int i = 0; i < n; i++) {
            out[i] = T[(int) (a[i] & 7)] * a[i] + i;   // getstatic T + laload + arithmetic
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
