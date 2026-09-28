/** Second miscellany: the cases whose old rava bodies lost information by
 *  routing through f64 or by using a different order of operations. */
public class NumMisc2 {
    static void p(String k, Object v) { System.out.println(k + "=" + v); }

    public static void main(String[] args) {
        // compareTo on longs beyond 2^53 — an f64 route collapses these
        long a = 9007199254740993L, b = 9007199254740992L;
        p("L_cmpTo_53", Long.valueOf(a).compareTo(Long.valueOf(b)));
        p("L_cmp_53", Long.compare(a, b));
        p("L_cmpTo_53r", Long.valueOf(b).compareTo(Long.valueOf(a)));
        p("L_cmpTo_big", Long.valueOf(Long.MAX_VALUE).compareTo(Long.valueOf(Long.MAX_VALUE - 1)));

        // instance hashCode on a boxed float — floatToIntBits, not the double bits
        p("F_obj_hash", Float.valueOf(1.5f).hashCode());
        p("F_obj_hash_p1", Float.valueOf(0.1f).hashCode());
        p("D_obj_hash", Double.valueOf(1.5).hashCode());
        p("F_obj_hash_nan", Float.valueOf(Float.NaN).hashCode());

        // toRadians/toDegrees: the JLS fixes the ORDER (angdeg / 180.0 * PI)
        double worst = 0.0;
        for (int i = 1; i <= 20000; i++) {
            double d = i * 0.001;
            worst += Math.toRadians(d) + Math.toDegrees(d);
        }
        p("radians_sum", worst);
        p("toRadians_1", Math.toRadians(1.0));
        p("toRadians_181", Math.toRadians(181.0));
        p("toRadians_0p3", Math.toRadians(0.3));
        p("toDegrees_1", Math.toDegrees(1.0));
        p("toDegrees_0p3", Math.toDegrees(0.3));

        // clamp keeps long precision
        p("clamp_long", Math.clamp(9007199254740993L, 0L, Long.MAX_VALUE));
        p("clamp_int", Math.clamp(5L, 1, 3));
        p("clamp_double", Math.clamp(1.5, 0.0, 1.0));

        System.out.println("DONE");
    }
}
