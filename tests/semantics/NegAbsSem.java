// Float and double negation and Math.abs in compiled code (sign masks from memory): signed
// zeros, NaN, infinities, and values live across the operation in many registers.
public class NegAbsSem {
    static long mix(float a, float b, double c, double d) {
        float x = -a, y = Math.abs(b), z = -(a * b), w = Math.abs(-b);
        double p = -c, q = Math.abs(d), r = -(c - d), s = Math.abs(-c);
        return Float.floatToRawIntBits(x) * 31L + Float.floatToRawIntBits(y) * 17 + Float.floatToRawIntBits(z) * 7 + Float.floatToRawIntBits(w)
            + Double.doubleToRawLongBits(p) * 3 + Double.doubleToRawLongBits(q) * 5 + Double.doubleToRawLongBits(r) * 11 + Double.doubleToRawLongBits(s) + (long) (x + y + z + w + p + q + r + s);
    }

    public static void main(String[] args) {
        float[] fs = {0f, -0f, 1.5f, -2.25f, Float.NaN, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY, Float.MIN_VALUE, -Float.MAX_VALUE};
        double[] ds = {0.0, -0.0, 1.5, -2.25, Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.MIN_VALUE, -Double.MAX_VALUE};
        long h = 0;
        for (int i = 0; i < 100000; i++)
            for (int j = 0; j < fs.length; j++) h = h * 31 + mix(fs[j], fs[(j + i) % fs.length], ds[j], ds[(j + 3) % ds.length]);
        System.out.println(h);
    }
}
