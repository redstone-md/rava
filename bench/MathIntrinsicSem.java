// Math.min/max/abs/floor/ceil/rint/sqrt on edge values, hot enough to run compiled.
public class MathIntrinsicSem {
    static final double[] D = {0.0, -0.0, 1.5, -1.5, Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.MIN_VALUE, -2.5, 2.5};
    static final float[] F = {0.0f, -0.0f, 1.5f, -1.5f, Float.NaN, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY, -2.5f};
    static final int[] I = {0, 1, -1, Integer.MIN_VALUE, Integer.MAX_VALUE, 42, -42};
    static final long[] J = {0L, 1L, -1L, Long.MIN_VALUE, Long.MAX_VALUE, 42L, -42L};

    static long mix(long h, long v) { return h * 31 + v; }

    static long round() {
        long h = 0;
        for (double a : D) {
            h = mix(h, Double.doubleToRawLongBits(Math.abs(a)));
            h = mix(h, Double.doubleToRawLongBits(Math.floor(a)));
            h = mix(h, Double.doubleToRawLongBits(Math.ceil(a)));
            h = mix(h, Double.doubleToRawLongBits(Math.rint(a)));
            h = mix(h, Double.doubleToRawLongBits(Math.sqrt(a)));
            for (double b : D) {
                h = mix(h, Double.doubleToRawLongBits(Math.min(a, b)));
                h = mix(h, Double.doubleToRawLongBits(Math.max(a, b)));
            }
        }
        for (float a : F) {
            h = mix(h, Float.floatToRawIntBits(Math.abs(a)));
            for (float b : F) {
                h = mix(h, Float.floatToRawIntBits(Math.min(a, b)));
                h = mix(h, Float.floatToRawIntBits(Math.max(a, b)));
            }
        }
        for (int a : I) {
            h = mix(h, Math.abs(a));
            for (int b : I) h = mix(h, Math.min(a, b) ^ ((long) Math.max(a, b) << 7));
        }
        for (long a : J) {
            h = mix(h, Math.abs(a));
            for (long b : J) h = mix(h, Math.min(a, b) ^ (Math.max(a, b) >>> 3));
        }
        return h;
    }

    public static void main(String[] args) {
        long first = round(), same = 0;
        for (int i = 0; i < 3000; i++) if (round() == first) same++;
        System.out.println("hash=" + Long.toHexString(first) + " stable=" + same);
    }
}
