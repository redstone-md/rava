// Counted loops whose array bounds checks optimized code hoists before the loop: every outcome
// matches the checks in the loop, including loops that run no iteration, loops that fail
// part-way (after side effects of the iterations before), negative starts and limits that
// are not constants, with the loop kept hot in between.
public class RangeCheckLoopSem {
    static long sum(int[] a, int from, int to, int[] out) {
        long s = 0;
        for (int i = from; i < to; i++) {
            s += a[i];
            out[i] = (int) s;
        }
        return s;
    }
    static long dist(long[] p, long[] q) {
        long d = 0;
        for (int i = 0; i < 7; i++) {
            long x = p[i] - q[i];
            d += x * x;
        }
        return d;
    }
    static long lengthLoop(int[] a) {
        long s = 0;
        for (int i = 0; i < a.length; i++) s += a[i] * (long) i;
        return s;
    }
    static String run(int[] a, int from, int to, int[] out) {
        java.util.Arrays.fill(out, 0);
        try {
            return sum(a, from, to, out) + " " + java.util.Arrays.stream(out).sum();
        } catch (ArrayIndexOutOfBoundsException e) {
            return "AIOOBE " + e.getMessage() + " " + java.util.Arrays.stream(out).sum();
        } catch (NullPointerException e) {
            return "NPE " + java.util.Arrays.stream(out).sum();
        }
    }
    public static void main(String[] args) {
        int[] a = new int[64], out = new int[64];
        for (int i = 0; i < a.length; i++) a[i] = i * 3 + 1;
        long[] p = { 1, 2, 3, 4, 5, 6, 7, 8 }, q = { 7, 6, 5, 4, 3, 2, 1 };
        long acc = 0;
        for (int r = 0; r < 100000; r++) {
            acc += sum(a, r % 7, 40 + r % 20, out) + dist(p, q) + lengthLoop(a);
        }
        System.out.println(acc);
        System.out.println(lengthLoop(new int[0]) + " " + lengthLoop(new int[] { 5 }));
        try { lengthLoop(null); } catch (NullPointerException e) { System.out.println("lengthLoop NPE"); }
        System.out.println(run(a, 0, 64, out));
        System.out.println(run(a, 10, 10, out));
        System.out.println(run(new int[0], 0, 0, out));
        System.out.println(run(a, 30, 70, new int[80]));
        System.out.println(run(a, 50, 60, new int[55]));
        System.out.println(run(a, -3, 5, out));
        System.out.println(run(null, 0, 0, out));
        System.out.println(run(null, 0, 3, out));
        System.out.println(run(a, 60, Integer.MIN_VALUE, out));
        try { System.out.println(dist(p, new long[3])); } catch (ArrayIndexOutOfBoundsException e) { System.out.println("dist " + e.getMessage()); }
        for (int r = 0; r < 100000; r++) {
            acc += sum(a, r % 7, 40 + r % 20, out) + dist(p, q);
        }
        System.out.println(acc + " " + run(a, 5, 66, new int[70]));
    }
}
