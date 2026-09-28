// Pure int/long throughput (NO double, NO calls) — the LLVM-AOT numeric-tier target.
// A xorshift-style long fold: exercises lload/lstore, imul, i2l, ladd, lshl/lushr, lxor.
public class PureLong {
    static long hot(int iters) {
        long acc = 1;
        for (int i = 0; i < iters; i++) {
            acc += (long) (i * 31);
            acc ^= acc << 13;
            acc ^= acc >>> 7;
            acc ^= acc << 17;
        }
        return acc;
    }

    public static void main(String[] a) {
        for (int w = 0; w < 15; w++) hot(2_000_000);          // warmup (past the compile threshold)
        long t0 = System.nanoTime();
        long s = hot(50_000_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
