/** Branches on long, float and double comparisons, including NaN, signed zeros and infinities. */
public class OptCompareSem {
    static int dbl(double a, double b) {
        int r = 0;
        if (a < b) r |= 1;
        if (a <= b) r |= 2;
        if (a > b) r |= 4;
        if (a >= b) r |= 8;
        if (a == b) r |= 16;
        if (a != b) r |= 32;
        if (!(a < b)) r |= 64;
        if (!(a > b)) r |= 128;
        return r;
    }

    static int flt(float a, float b) {
        int r = 0;
        if (a < b) r |= 1;
        if (a <= b) r |= 2;
        if (a > b) r |= 4;
        if (a >= b) r |= 8;
        if (a == b) r |= 16;
        if (a != b) r |= 32;
        if (!(a <= b)) r |= 64;
        if (!(a >= b)) r |= 128;
        return r;
    }

    static int lng(long a, long b) {
        int r = 0;
        if (a < b) r |= 1;
        if (a <= b) r |= 2;
        if (a > b) r |= 4;
        if (a >= b) r |= 8;
        if (a == b) r |= 16;
        if (a != b) r |= 32;
        return r;
    }

    public static void main(String[] args) {
        double[] ds = {0.0, -0.0, 1.5, -2.25, Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.MIN_VALUE};
        long[] ls = {0, 1, -1, Long.MAX_VALUE, Long.MIN_VALUE, 42};
        long h = 0;
        for (int rep = 0; rep < 3000; rep++) {
            for (double a : ds) for (double b : ds) h = h * 31 + dbl(a, b);
            for (double a : ds) for (double b : ds) h = h * 31 + flt((float) a, (float) b);
            for (long a : ls) for (long b : ls) h = h * 31 + lng(a, b);
        }
        StringBuilder sb = new StringBuilder();
        for (double a : ds) for (double b : ds) sb.append(dbl(a, b)).append(',').append(flt((float) a, (float) b)).append(' ');
        System.out.println(sb);
        System.out.println("hash " + h);
    }
}
