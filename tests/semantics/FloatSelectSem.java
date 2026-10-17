// Conditional selects on float and double comparisons (if-conversion of `a < b ? x : y`):
// every order, both branch layouts javac emits, NaN, signed zeros and infinities, hot enough
// for the optimizing tier.
public class FloatSelectSem {
    static int floor(double v) { int i = (int) v; return v < i ? i - 1 : i; }
    static int ceil(float v) { int i = (int) v; return v > i ? i + 1 : i; }
    static int lt(double a, double b) { return a < b ? 1 : 2; }
    static int le(double a, double b) { return a <= b ? 3 : 4; }
    static int gt(double a, double b) { return a > b ? 5 : 6; }
    static int ge(double a, double b) { return a >= b ? 7 : 8; }
    static int notLt(double a, double b) { return !(a < b) ? 9 : 10; }
    static int notGe(float a, float b) { return !(a >= b) ? 11 : 12; }
    static long pick(float a, float b, long x, long y) { return a <= b ? x : y; }
    static int nested(double a, double b) { return a > 0 ? (b < a ? 13 : 14) : 15; }
    static int clamp(double v) { int i = (int) v; i = v > 100 ? 100 : i; return v < -100 ? -100 : i; }

    public static void main(String[] args) {
        double[] ds = {0.0, -0.0, 1.0, -1.0, 0.5, -0.5, 2.75, -2.75, 1e10, -1e10, 150.5, -150.5,
                Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.MIN_VALUE, -Double.MIN_VALUE};
        float[] fs = {0f, -0f, 1f, -1f, 0.5f, -0.5f, 3.25f, -3.25f, Float.NaN, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY};
        long sum = 0;
        StringBuilder first = new StringBuilder();
        for (int round = 0; round < 3000; round++) {
            for (double a : ds) {
                sum = sum * 31 + floor(a) + clamp(a);
                for (double b : ds) {
                    int r = lt(a, b) + le(a, b) * 10 + gt(a, b) * 100 + ge(a, b) * 1000 + notLt(a, b) * 10000 + nested(a, b) * 100000;
                    sum = sum * 31 + r;
                    if (round == 0 && a == 1.0 && first.length() < 400) first.append(r).append(' ');
                }
            }
            for (float a : fs) {
                sum = sum * 31 + ceil(a);
                for (float b : fs) sum = sum * 31 + notGe(a, b) + pick(a, b, 17, 19);
            }
        }
        System.out.println(first.toString().trim());
        System.out.println("floor " + floor(-2.5) + " " + floor(2.5) + " " + floor(-0.0) + " " + floor(Double.NaN) + " " + floor(-1e20));
        System.out.println("ceil " + ceil(-2.5f) + " " + ceil(2.5f) + " " + ceil(Float.NaN) + " " + ceil(Float.POSITIVE_INFINITY));
        System.out.println("sum " + sum);
    }
}
