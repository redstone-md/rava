// Generic numeric throughput — tight long/double loop. JIT target (Phase C).
public class ArithLoop {
    static long hot(int iters) {
        long acc = 0; double d = 1.0;
        for (int i = 0; i < iters; i++) {
            acc += (long) (i * 31 + (acc & 0xFFFF));
            acc ^= acc << 13; acc ^= acc >>> 7; acc ^= acc << 17;
            d = d * 1.0000001 + (i & 7);
            acc += (long) d;
        }
        return acc ^ (long) d;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 15; w++) hot(2_000_000);          // warmup
        long t0 = System.nanoTime();
        long s = hot(20_000_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
