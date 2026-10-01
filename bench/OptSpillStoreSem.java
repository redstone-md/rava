/** Array stores whose index is spilled (more live values than registers): byte[] and boolean[]
 *  stores (bastore tests the array's class through a scratch register the spilled index also
 *  uses) and float/double stores of constants (loaded through that register). */
public class OptSpillStoreSem {
    static long run(byte[] bytes, boolean[] bools, double[] ds, float[] fs, int n, int seed) {
        int a = seed, b = seed * 3, c = seed ^ 5, d = seed + 7, e = seed * 11, f = seed - 13, g = seed ^ 17, h = seed + 19;
        int i2 = seed * 23, j2 = seed + 29, k2 = seed ^ 31, l2 = seed * 37, m2 = seed + 41, n2 = seed ^ 43;
        long s = 0;
        for (int i = 0; i < n; i++) {
            int idx = (i * 7 + a + b + c + d + e + f + g + h + i2 + j2 + k2 + l2 + m2 + n2) & 63;
            bytes[idx] = (byte) (i + a);
            bools[(idx + 1) & 63] = ((i + b) & 3) == 0 ? true : (boolean) (((i ^ c) & 1) == 1);
            ds[(idx + 2) & 63] = 1.5;
            fs[(idx + 3) & 63] = 2.25f;
            s += bytes[idx] + (bools[(idx + 1) & 63] ? 1 : 0) + (long) ds[(idx + 2) & 63] + (long) (fs[(idx + 3) & 63] * 4);
            a += b; b ^= c; c += d; d ^= e; e += f; f ^= g; g += h; h ^= i2; i2 += j2; j2 ^= k2; k2 += l2; l2 ^= m2; m2 += n2; n2 ^= i;
        }
        return s + a + b + c + d + e + f + g + h + i2 + j2 + k2 + l2 + m2 + n2;
    }

    public static void main(String[] args) {
        byte[] bytes = new byte[64];
        boolean[] bools = new boolean[64];
        double[] ds = new double[64];
        float[] fs = new float[64];
        long t = 0;
        for (int r = 0; r < 20000; r++) t += run(bytes, bools, ds, fs, 50, r);
        long sum = 0;
        for (int i = 0; i < 64; i++) sum = sum * 31 + bytes[i] + (bools[i] ? 7 : 3) + (long) ds[i] + (long) fs[i];
        System.out.println(t + " " + sum);
    }
}
