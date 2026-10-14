// Multiplication by constants, which optimized code makes a shift (powers of two) or one `lea`
// (3, 5, 9): int and long, negative operands, results that overflow, and the largest powers.
public class MulConstSem {
    static int ints(int x) {
        return x * 2 ^ x * 4 * 7 ^ x * 8 ^ x * 64 ^ x * 1024 ^ x * 0x40000000 ^ x * 3 ^ x * 5 ^ x * 9 ^ x * -4 ^ x * 6;
    }

    static long longs(long x) {
        return x * 2 ^ x * 8 ^ x * 64 ^ x * 0x40000000L ^ x * 3 ^ x * 5 ^ x * 9 ^ x * -8 ^ x * 10 ^ (x * 1024) >>> 3;
    }

    static long index(long base, int i) {
        return base + i * 64L + (long) (i * 9) * 3;
    }

    public static void main(String[] args) {
        int[] xs = {0, 1, -1, 7, -7, 0x7fffffff, 0x80000000, 0x12345678, 0x40000001, -0x3ffffffe};
        long hi = 0, hl = 0, hx = 0;
        for (int r = 0; r < 30000; r++) {
            for (int x : xs) {
                hi = hi * 31 + ints(x + r);
                hl = hl * 31 + longs(((long) x << 17) + r * 0x100000001L);
                hx = hx * 31 + index(x, r ^ x);
            }
        }
        System.out.println(hi + " " + hl + " " + hx);
    }
}
