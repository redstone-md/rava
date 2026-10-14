// Early returns computed in floating point before the prologue (optimized code's fast exit): a
// recursion's base case returning double or float arithmetic on its arguments, constants and
// converted integers, including NaN, infinities, negative zero and float/double conversions.
public class FastExitFloatSem {
    static double mixed(int depth, double x, long k, double y) {
        if (depth == 0) return x * y + k;
        return mixed(depth - 1, x * 0.5, k + depth, y + 1) - mixed(depth - 1, y, k ^ 3, x);
    }

    static float half(int n, float f, int i) {
        if (n <= 0) return f / 2.5f - i;
        return half(n - 1, f + 1, i * 3) + half(n - 2, f * -1.5f, i - 7);
    }

    static double conv(int n, float f, long l) {
        if (n < 1) return (double) f - l + 0.25;
        return conv(n - 1, f * 2, l + 1) * 0.5;
    }

    static float back(int n, double d) {
        if (n == 0) return (float) d;
        return back(n - 1, d * 3.3) + 1;
    }

    static double constant(int n, double d) {
        if (n != 0) return 1e300 * 1e10;
        return d - d;
    }

    public static void main(String[] args) {
        double s = 0;
        for (int r = 0; r < 40; r++) {
            s += mixed(12, 1.5 + r, 7, 2.5);
            s += half(14, r * 0.75f, r);
            s += conv(r & 7, r * 0.1f, -r);
            s += back(r & 3, r - 20.0);
        }
        System.out.println(s);
        System.out.println(constant(1, 0) + " " + constant(0, Double.NaN) + " " + constant(0, Double.POSITIVE_INFINITY) + " " + (1 / constant(0, -0.0)));
        System.out.println(half(0, -0.0f, 0) + " " + half(0, Float.NaN, 1) + " " + back(0, 1e40) + " " + back(0, -1e-50));
        System.out.println(conv(0, Float.MAX_VALUE, Long.MIN_VALUE) + " " + mixed(0, -0.0, 0, 5));
    }
}
