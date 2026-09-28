import java.util.Locale;

/** %f/%e/%g across every precision 0..12 over a deterministic value set. */
public class FmtSweep {
    static long s = 0xB5026F5AA96619E9L;
    static long next() { s ^= s << 13; s ^= s >>> 7; s ^= s << 17; return s; }

    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();
        double[] fixed = {
            0.0, -0.0, 1.0, -1.0, 0.5, 0.25, 0.125, 2.005, 9.95, 1e-5, 1e-4, 1e-3,
            1e7, 1e20, 1e-20, 1.0 / 3.0, 2.0 / 3.0, 123456789.987654321,
            Double.MIN_VALUE, Double.MIN_NORMAL, Double.MAX_VALUE, 99.995, -99.995,
            0.0001, 999999.5, 1234.5678, 1e15 + 0.25,
        };
        for (double d : fixed) emit(sb, d);
        for (int i = 0; i < 600; i++) {
            double d = Double.longBitsToDouble(next());
            if (!Double.isFinite(d)) d = (next() % 100000000L) / 10000.0;
            emit(sb, d);
        }
        for (int i = 0; i < 600; i++) emit(sb, (next() % 1000000L) / 1000.0);
        System.out.print(sb);
        System.out.println("DONE");
    }

    static void emit(StringBuilder sb, double d) {
        for (int p = 0; p <= 12; p++) {
            sb.append(String.format(Locale.ROOT, "%." + p + "f", d)).append('|');
            sb.append(String.format(Locale.ROOT, "%." + p + "e", d)).append('|');
            sb.append(String.format(Locale.ROOT, "%." + p + "g", d)).append('|');
        }
        sb.append(String.format(Locale.ROOT, "%f|%e|%g|%s", d, d, d, d)).append('\n');
    }
}
