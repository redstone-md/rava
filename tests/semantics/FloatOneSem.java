// Multiplying or dividing by a literal 1 (inlined arguments) gives the operand exactly: -0,
// infinities and NaN included.
public class FloatOneSem {
    static float mf(float x, float k) { return x * k; }
    static float df(float x, float k) { return x / k; }
    static double md(double x, double k) { return k * x; }
    static double dd(double x, double k) { return x / k; }

    static long run(float[] fs, double[] ds) {
        long h = 0;
        for (float f : fs) h = h * 31 + Float.floatToRawIntBits(mf(f, 1f)) + Float.floatToRawIntBits(df(f, 1f));
        for (double d : ds) h = h * 31 + Double.doubleToRawLongBits(md(d, 1.0)) + Double.doubleToRawLongBits(dd(d, 1.0));
        return h;
    }

    public static void main(String[] args) {
        float[] fs = {0f, -0f, 1.5f, -3.25f, Float.NaN, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY, Float.MIN_VALUE, Float.MAX_VALUE};
        double[] ds = {0.0, -0.0, 1.5, -3.25, Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.MIN_VALUE, Double.MAX_VALUE};
        long h = 0;
        for (int i = 0; i < 200000; i++) h += run(fs, ds);
        System.out.println(h);
    }
}
