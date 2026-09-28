public class PureMap {
    static void map(long[] out, long[] in, int n) {
        for (int i = 0; i < n; i++) {
            long x = in[i];
            out[i] = x * x + (x >>> 13) - (x << 7);
        }
    }
    public static void main(String[] a) {
        int n = 20_000_000;
        long[] in = new long[n], out = new long[n];
        for (int i = 0; i < n; i++) in[i] = (long) i * 2654435761L + 1;
        for (int w = 0; w < 20; w++) map(out, in, n);       // warmup
        long t0 = System.nanoTime();
        for (int r = 0; r < 10; r++) map(out, in, n);        // TIMED: 10 pure map passes
        long ns = (System.nanoTime() - t0) / 10;
        long s = 0; for (int i = 0; i < n; i++) s += out[i];
        System.out.println("SUM=" + s);
        System.out.println("MAPNS=" + ns);
    }
}
