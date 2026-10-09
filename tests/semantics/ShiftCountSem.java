// Shift counts that went through a narrowing conversion (l2i, i2b, i2c, i2s) keep their low
// bits, and the conversions themselves read registers directly: negative and wide counts, both
// shift widths, conversions of values in registers and of spilled ones.
public class ShiftCountSem {
    static int ints(int x, long c, int d) {
        return (x << (int) c) ^ (x >> (byte) d) ^ (x >>> (char) d) ^ (x << (short) (d * 7)) ^ (x >>> (int) (c >> 32));
    }

    static long longs(long x, long c, int d) {
        return (x << (int) c) ^ (x >> (byte) d) ^ (x >>> (char) d) ^ (x << (short) (d * 7)) ^ (x >>> (byte) (c >> 32));
    }

    static long conversions(long a, int b) {
        int i = (int) a;
        byte y = (byte) b;
        char c = (char) b;
        short s = (short) b;
        long back = i;
        return back * 3 + y + c * 5 + s * 7 + (long) (byte) i + (long) (char) i;
    }

    public static void main(String[] args) {
        long acc = 0;
        long[] cs = { 0, 1, 31, 32, 33, 63, 64, -1, -31, -33, 0x1_0000_0003L, 0xFFFF_FFFF_0000_0005L, Long.MIN_VALUE + 7 };
        int[] ds = { 0, 1, 5, 31, 32, 255, 256, -1, -129, 0x12345, -0x7654321 };
        for (int r = 0; r < 20000; r++) {
            long c = cs[r % cs.length];
            int d = ds[r % ds.length];
            int x = 0x9E3779B9 * (r + 1);
            acc += ints(x, c, d);
            acc ^= longs(0x9E3779B97F4A7C15L * (r + 1), c, d);
            acc += conversions(c * (r + 3), d + r);
        }
        System.out.println("acc " + acc);
    }
}
