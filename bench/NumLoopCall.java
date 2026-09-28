// M3 T2 step 3c: a counted-loop body that CALLS a pure (offloadable) static method. Forkable iff
// the callee is offloadable + alloc-free + whole-tree-compiled (compiled → per-thread tlcache for
// resolve/getstatic, avoiding the shared per-class RefCells an interpreted callee races). `mix` is
// hot (called n× per fill) so it compiles; `fill` is called only 8× so it stays interpreted (the
// counted-loop fork fires on the interpreted goto).
public class NumLoopCall {
    static long mix(long v, long i) {
        return v * v + i * 3 + 7;
    }

    static void fill(long[] out, long[] a, int n) {
        for (int i = 0; i < n; i++) {
            out[i] = mix(a[i], (long) i);
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
