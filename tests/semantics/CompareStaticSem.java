// lcmp and float compares materialized as -1/0/1 (Long.compare, Double.compare's NaN cases),
// and static stores of each type (narrowed on the way in), in a loop with many live values.
public class CompareStaticSem {
    static byte sb; static short ss; static char sc; static boolean sz; static int si;
    static long sl; static float sf; static double sd; static Object so;

    static int cmp(long a, long b) { int c = a < b ? -1 : a == b ? 0 : 1; return c; }

    static long run(long[] ls, double[] ds, int n) {
        long acc = 0, x1 = 1, x2 = 2, x3 = 3, x4 = 4, x5 = 5, x6 = 6;
        for (int i = 0; i < n; i++) {
            long a = ls[i & 7], b = ls[(i + 3) & 7];
            double p = ds[i & 7], q = ds[(i + 5) & 7];
            int c1 = cmp(a, b), c2 = Long.compare(b, a);
            int c3 = p < q ? -1 : p > q ? 1 : p == q ? 0 : 2;
            int c4 = (float) p > (float) q ? 1 : (float) p < (float) q ? -1 : 0;
            sb = (byte) (a + i); ss = (short) (b * 3); sc = (char) (a ^ b); sz = (i & 1) == 0;
            si = (int) a; sl = b; sf = (float) p; sd = q; so = (i & 3) == 0 ? null : ls;
            acc += c1 * 7 + c2 * 11 + c3 * 13 + c4 * 17 + sb + ss + sc + (sz ? 1 : 0) + si + sl + (long) sf + (long) sd + (so == null ? 1 : 2);
            x1 += acc; x2 ^= x1; x3 += x2 >>> 1; x4 ^= x3; x5 += x4; x6 ^= x5 + i;
        }
        return acc ^ x1 ^ x2 * 3 ^ x3 * 5 ^ x4 * 7 ^ x5 * 11 ^ x6 * 13;
    }

    public static void main(String[] args) {
        long[] ls = {0, -1, Long.MIN_VALUE, Long.MAX_VALUE, 5, 5, -7, 1L << 40};
        double[] ds = {0.0, -0.0, Double.NaN, 1.5, -1e300, Double.POSITIVE_INFINITY, 3.25, Float.NaN};
        long s = 0;
        for (int r = 0; r < 300; r++) s = s * 31 + run(ls, ds, 10000);
        System.out.println(s + " " + sb + " " + ss + " " + (int) sc + " " + sz + " " + si + " " + sl + " " + sf + " " + sd + " " + (so != null));
    }
}
