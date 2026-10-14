// Repeated numeric intrinsics across branches retain modes, widths, NaNs and signed zero;
// raw bit conversions preserve NaN payloads, and volatile reads still observe intervening stores.
import java.util.concurrent.atomic.AtomicInteger;

public class NumericGvnSem {
    static final long[] DOUBLES = {
        0L, Long.MIN_VALUE, 0x3ff8000000000000L, 0xbff8000000000000L,
        0x3fe0000000000000L, 0xbfe0000000000000L, 1L, 0x8000000000000001L,
        0x7ff0000000000000L, 0xfff0000000000000L, 0x7ff8000000001234L,
        0xfff8000000005678L, 0x7ff0000000001234L, 0x4330000000000000L
    };
    static final int[] FLOATS = {
        0, Integer.MIN_VALUE, 0x3fc00000, 0xbfc00000, 1, 0x80000001,
        0x7f800000, 0xff800000, 0x7fc01234, 0xffc05678, 0x7f801234
    };
    static final AtomicInteger CELL = new AtomicInteger(7);

    static long mix(long h, long x) { return (h ^ x) * 0x9e3779b97f4a7c15L; }
    static long bits(double x) { return Double.doubleToLongBits(x); }
    static int bits(float x) { return Float.floatToIntBits(x); }

    static long numeric(double x, double y, float f, float g, int i, long l) {
        long h = bits(Math.floor(x));
        h = mix(h, bits(Math.ceil(x)));
        h = mix(h, bits(Math.rint(x)));
        h = mix(h, bits(Math.sqrt(x)));
        h = mix(h, bits(Math.abs(x)));
        h = mix(h, bits(Math.min(x, y)));
        h = mix(h, bits(Math.max(x, y)));
        h = mix(h, bits(Math.min(y, x)));
        h = mix(h, bits(Math.abs(f)));
        h = mix(h, bits(Math.min(f, g)));
        h = mix(h, bits(Math.max(f, g)));
        h = mix(h, Math.abs(i));
        h = mix(h, Math.abs(l));
        h = mix(h, Math.min(i, (int) l));
        h = mix(h, Math.max(i, (int) l));
        h = mix(h, Math.min(l, (long) i));
        h = mix(h, Math.max(l, (long) i));
        h = mix(h, Integer.bitCount(i));
        h = mix(h, Integer.numberOfLeadingZeros(i));
        h = mix(h, Integer.numberOfTrailingZeros(i));
        h = mix(h, Long.bitCount(l));
        h = mix(h, Long.numberOfLeadingZeros(l));
        h = mix(h, Long.numberOfTrailingZeros(l));
        if ((i & 1) == 0) h = mix(h, 17); else h = mix(h, -31);
        // The same computations after a merge, including distinct rounding modes.
        h = mix(h, bits(Math.floor(x)));
        h = mix(h, bits(Math.ceil(x)));
        h = mix(h, bits(Math.rint(x)));
        h = mix(h, bits(Math.sqrt(x)));
        h = mix(h, bits(Math.abs(x)));
        h = mix(h, bits(Math.min(x, y)));
        h = mix(h, bits(Math.max(x, y)));
        h = mix(h, bits(Math.abs(f)));
        h = mix(h, bits(Math.min(f, g)));
        h = mix(h, bits(Math.max(f, g)));
        h = mix(h, Math.abs(i));
        h = mix(h, Math.abs(l));
        h = mix(h, Integer.bitCount(i));
        h = mix(h, Integer.numberOfLeadingZeros(i));
        h = mix(h, Integer.numberOfTrailingZeros(i));
        h = mix(h, Long.bitCount(l));
        h = mix(h, Long.numberOfLeadingZeros(l));
        h = mix(h, Long.numberOfTrailingZeros(l));
        return h;
    }

    static long raw(long d, int f) {
        double x = Double.longBitsToDouble(d), y = Double.longBitsToDouble(d);
        float a = Float.intBitsToFloat(f), b = Float.intBitsToFloat(f);
        long h = Double.doubleToRawLongBits(x);
        h = mix(h, Double.doubleToRawLongBits(y));
        h = mix(h, Float.floatToRawIntBits(a));
        return mix(h, Float.floatToRawIntBits(b));
    }

    static int effects() {
        int a = CELL.get();
        CELL.set(a + 1);
        int b = CELL.get();
        return a * 31 + b;
    }

    public static void main(String[] args) {
        long result = 0, raw = 0, effects = 0;
        for (int n = 0; n < 40000; n++) {
            long d = DOUBLES[n % DOUBLES.length];
            long e = DOUBLES[(n * 3 + 1) % DOUBLES.length];
            int f = FLOATS[n % FLOATS.length];
            int g = FLOATS[(n * 3 + 1) % FLOATS.length];
            int i = n % 7 == 0 ? Integer.MIN_VALUE : n % 7 == 1 ? 0 : n * 0x1f123bb5;
            long l = n % 7 == 0 ? Long.MIN_VALUE : n % 7 == 1 ? 0 : n * 0x123456789abcdefL;
            result += numeric(Double.longBitsToDouble(d), Double.longBitsToDouble(e),
                              Float.intBitsToFloat(f), Float.intBitsToFloat(g), i, l);
            raw += raw(d, f);
            effects += effects();
        }
        System.out.println("numeric=" + result + " raw=" + raw);
        System.out.println("effects=" + effects + " cell=" + CELL.get());
    }
}
