/** Arithmetic the optimizing tier rewrites: division by powers of two, multiplication by
 *  constants (overflow), and repeated subexpressions. */
public class OptArithSem {
    static double dq(double x) { return x / 4.0 + x / 1024.0 - x / 0.5 + x / -8.0 + x / 3.0; }
    static float fq(float x) { return x / 4f + x / 0.25f - x / 65536f + x / -2f; }
    static long mul(long a, int b) { return a * 31 + a * -7 + (long) (b * 1_000_003) + b * 17L + a * 0x7fffffffL; }
    static int cse(int x, int y) { int a = x * 31 + y * 17, b = x * 31 + y * 17 + 1; return a ^ b ^ (x * 31); }
    public static void main(String[] args) {
        double[] ds = {0.0, -0.0, 1.0, -3.5, Double.MIN_VALUE, Double.MIN_NORMAL, Double.MAX_VALUE, Double.NaN,
                Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, 1e-310, 123456.789};
        long h = 0;
        for (int rep = 0; rep < 5000; rep++) {
            for (double d : ds) h = h * 31 + Double.doubleToRawLongBits(dq(d));
            for (double d : ds) h = h * 31 + Float.floatToRawIntBits(fq((float) d));
            for (int i = -3; i < 4; i++) h = h * 31 + mul(Long.MAX_VALUE - i, Integer.MIN_VALUE + i) + cse(i * 123457, rep);
        }
        StringBuilder sb = new StringBuilder();
        for (double d : ds) sb.append(dq(d)).append(' ').append(fq((float) d)).append(' ');
        System.out.println(sb);
        System.out.println("hash " + h);
    }
}
