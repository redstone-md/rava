// Math.multiplyHigh and unsignedMultiplyHigh: the high 64 bits of the 128-bit product, signed
// and unsigned, at the extremes, compiled; and Double.toString through DoubleToDecimal.
public class MulHighSem {
    static long run(long a, long b) {
        return Math.multiplyHigh(a, b) * 31 + Math.unsignedMultiplyHigh(a, b);
    }

    public static void main(String[] args) {
        long[] xs = {0, 1, -1, Long.MAX_VALUE, Long.MIN_VALUE, 0x123456789abcdefL, -0x7edcba9876543210L, 3, 1L << 32, -(1L << 33)};
        long h = 0;
        for (int r = 0; r < 100000; r++)
            for (long a : xs) for (long b : xs) h = h * 7 + run(a + r, b ^ r);
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 20000; i++) sb.setLength(0);
        double d = 0.1;
        for (int i = 0; i < 50000; i++) { d = d * 1.37 + 1e-7; if (d > 1e6) d /= 9.1; h += Double.toString(d).hashCode(); }
        System.out.println(h);
    }
}
