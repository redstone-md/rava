// M3 column-fork executor validation: a counted loop whose body calls a compiled OFFLOADABLE
// method that ALLOCATES a transient object (the worldgen shape minus the setup cost — findBiome
// allocates Pair/BlockPos). Tests the per-worker nursery region + call + optimistic gate + bit-
// exactness, without the ~200s MC worldgen setup. `make` is hot (compiles); `fill` stays
// interpreted (few calls) so the counted-loop fork fires on its interpreted goto.
public class NumLoopAlloc {
    static final class Box {
        long v;
        Box(long v) { this.v = v; }
    }

    static long make(long x) {
        Box b = new Box(x * x + 7);   // transient alloc — dies after v is read
        return b.v;
    }

    static void fill(long[] out, long[] a, int n) {
        for (int i = 0; i < n; i++) {
            out[i] = make(a[i]);
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
