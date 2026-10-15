// Math.min, max and abs (ints, longs, floats with NaN and -0.0, doubles) and class queries in a
// loop with more live values than registers: the values living across them are kept.
public class MinMaxLiveSem {
    static long run(int n, float[] fs, double[] ds, Class<?>[] cs) {
        long a = 1, b = 2, c = 3, d = 4, e = 5, f = 6, g = 7, h = 8, k = 9;
        for (int i = 0; i < n; i++) {
            int x = Math.max(i - 50, (int) (a & 63)) + Math.min(i, 7) + Math.abs(i - 100);
            long y = Math.max(b, c - i) - Math.min(d, e + i) + Math.abs(f - i * 3L);
            float p = Math.max(fs[i & 7], fs[(i + 3) & 7]) + Math.min(fs[(i + 1) & 7], fs[(i + 5) & 7]) + Math.abs(fs[(i + 2) & 7]);
            double q = Math.max(ds[i & 7], ds[(i + 4) & 7]) - Math.min(ds[(i + 6) & 7], ds[(i + 1) & 7]) + Math.abs(ds[(i + 3) & 7]);
            int cls = (cs[i & 3].isPrimitive() ? 1 : 0) + (cs[(i + 1) & 3].isInterface() ? 2 : 0);
            a += x; b ^= y; c += Float.floatToRawIntBits(p); d ^= Double.doubleToRawLongBits(q);
            e += cls; f ^= a + b; g += c ^ d; h ^= e + f + g; k += h >>> 3;
        }
        return a ^ b * 3 ^ c * 5 ^ d * 7 ^ e * 11 ^ f * 13 ^ g * 17 ^ h * 19 ^ k * 23;
    }

    public static void main(String[] args) {
        float[] fs = {0.0f, -0.0f, Float.NaN, 1.5f, -2.25f, Float.POSITIVE_INFINITY, Float.MIN_VALUE, -7f};
        double[] ds = {0.0, -0.0, Double.NaN, 3.5, -1e300, Double.NEGATIVE_INFINITY, Double.MIN_VALUE, 42};
        Class<?>[] cs = {int.class, Runnable.class, String.class, double.class};
        long s = 0;
        for (int r = 0; r < 300; r++) s = s * 31 + run(5000, fs, ds, cs);
        System.out.println(s + " " + Math.abs(Integer.MIN_VALUE) + " " + Math.abs(Long.MIN_VALUE));
    }
}
