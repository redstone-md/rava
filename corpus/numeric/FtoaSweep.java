/** Brute-force cross-check of Double.toString / Float.toString over a
 *  deterministic pseudo-random bit sweep plus the layout-boundary neighbourhoods. */
public class FtoaSweep {
    static long s = 0x9E3779B97F4A7C15L;
    static long next() { s ^= s << 13; s ^= s >>> 7; s ^= s << 17; return s; }

    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();

        // 1) raw bit patterns -> every exponent class, incl. subnormals/NaN/Inf
        for (int i = 0; i < 20000; i++) {
            sb.append(Double.toString(Double.longBitsToDouble(next()))).append('\n');
        }
        for (int i = 0; i < 20000; i++) {
            sb.append(Float.toString(Float.intBitsToFloat((int) next()))).append('\n');
        }

        // 2) small integers scaled by every power of ten in range: the plain-vs-
        //    scientific layout boundary and the "shortest is one digit" cases
        for (int e = -330; e <= 320; e++) {
            for (int m = 1; m <= 9; m++) {
                double d = Double.parseDouble(m + "e" + e);
                sb.append(Double.toString(d)).append('\n');
                sb.append(Double.toString(-d)).append('\n');
            }
        }
        for (int e = -50; e <= 40; e++) {
            for (int m = 1; m <= 9; m++) {
                float f = Float.parseFloat(m + "e" + e);
                sb.append(Float.toString(f)).append('\n');
            }
        }

        // 3) neighbourhoods of the extremes, where the p<2 rule bites
        long[] seeds = { 1L, 2L, 3L, 10L, 4503599627370495L, 4503599627370496L,
                         0x7FEFFFFFFFFFFFFFL, 0x7FEFFFFFFFFFFFFEL, 0x0010000000000000L,
                         0x000FFFFFFFFFFFFFL };
        for (long b : seeds) {
            for (int k = -3; k <= 3; k++) {
                sb.append(Double.toString(Double.longBitsToDouble(b + k))).append('\n');
            }
        }
        for (int b : new int[] { 1, 2, 3, 0x00800000, 0x007FFFFF, 0x7F7FFFFF, 0x3F800000 }) {
            for (int k = -3; k <= 3; k++) {
                sb.append(Float.toString(Float.intBitsToFloat(b + k))).append('\n');
            }
        }

        // 4) integral doubles across the plain/scientific boundary
        for (long v = 999990L; v <= 10000010L; v += 101) {
            sb.append(Double.toString((double) v)).append('\n');
        }
        for (int k = 0; k < 5000; k++) {
            sb.append(Double.toString(k / 1000.0)).append('\n');
            sb.append(Float.toString(k / 1000.0f)).append('\n');
        }

        System.out.print(sb);
        System.out.println("DONE");
    }
}
