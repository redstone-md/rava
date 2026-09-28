// Generic string workload — concat, hashCode, compare. Stresses string repr +
// the box-table path today (Phase A makes strings real heap objects).
public class StringOps {
    static long hot(int iters) {
        long acc = 0;
        for (int i = 0; i < iters; i++) {
            String s = "item" + (i & 1023) + "-" + (i % 7);
            acc += s.hashCode();
            acc ^= s.length();
            if (s.startsWith("item0")) acc += 3;
        }
        return acc;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 15; w++) hot(100_000);            // warmup
        long t0 = System.nanoTime();
        long s = hot(2_000_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
