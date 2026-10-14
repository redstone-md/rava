// Math.fma rounds the exact a * b + c once: special values, signed zeros, overflow and cases
// where rounding the product first would give another result. Hot, too, for the compilers.
public class FmaSem {
    static final double[] D = {0.0, -0.0, 1.0, -1.0, 0.1, 1e308, -1e308, 1e-308, 4.9e-324, Double.MAX_VALUE, Double.MIN_NORMAL,
            Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, 1.0 + Math.ulp(1.0), 1.0 - Math.ulp(1.0) / 2, 3.0, 1.0 / 3};
    static final float[] F = {0.0f, -0.0f, 1.0f, -1.0f, 0.1f, 3e38f, -3e38f, 1e-38f, 1.4e-45f, Float.MAX_VALUE, Float.MIN_NORMAL,
            Float.NaN, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY, 1.0f + Math.ulp(1.0f), 1.0f - Math.ulp(1.0f) / 2, 3.0f, 1.0f / 3};

    static long hashD() {
        long hd = 0;
        for (double a : D) for (double b : D) for (double c : D) hd = hd * 31 + Double.doubleToRawLongBits(Math.fma(a, b, c));
        return hd;
    }

    static long hashF() {
        long hf = 0;
        for (float a : F) for (float b : F) for (float c : F) hf = hf * 31 + Float.floatToRawIntBits(Math.fma(a, b, c));
        return hf;
    }

    public static void main(String[] args) {
        System.out.println("double " + hashD());
        System.out.println("float " + hashF());
        // Again once the compilers have them (an intrinsic in optimized code).
        long hd = 0, hf = 0;
        for (int i = 0; i < 400; i++) {
            hd = hashD();
            hf = hashF();
        }
        System.out.println("compiled double " + hd + " float " + hf);
        // Where the unfused result differs.
        double x = 1.0 + Math.ulp(1.0), y = 1.0 - Math.ulp(1.0);
        System.out.println("fused " + Math.fma(x, y, -1.0) + " unfused " + (x * y - 1.0));
        float fx = 1.0f + Math.ulp(1.0f), fy = 1.0f - Math.ulp(1.0f);
        System.out.println("fused " + Math.fma(fx, fy, -1.0f) + " unfused " + (fx * fy - 1.0f));
        double acc = 0;
        float facc = 0;
        for (int i = 0; i < 300_000; i++) {
            acc = Math.fma(acc, 0.999, i * 1e-3);
            facc = Math.fma(facc, 0.999f, i * 1e-3f);
        }
        System.out.println("hot " + acc + " " + facc);
    }
}
