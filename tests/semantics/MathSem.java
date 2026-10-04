// Math.sin/cos/tan/exp/log: special values exactly, every result within 1 ulp of StrictMath's
// (fdlibm), and the same result from cold (interpreted or baseline) and hot (optimized) code.
import java.util.function.DoubleUnaryOperator;

public class MathSem {
    static final double[] SPECIAL = { 0.0, -0.0, Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY,
        1.0, -1.0, Double.MIN_VALUE, Double.MAX_VALUE, 709.78, 709.79, -745.2, -745.1, 1e-300, Math.PI, Math.PI / 2 };

    static double sin(double x) { return Math.sin(x); }
    static double cos(double x) { return Math.cos(x); }
    static double tan(double x) { return Math.tan(x); }
    static double exp(double x) { return Math.exp(x); }
    static double log(double x) { return Math.log(x); }

    static boolean within1ulp(double a, double b) {
        if (Double.isNaN(a) || Double.isNaN(b)) return Double.isNaN(a) && Double.isNaN(b);
        if (a == b) return true;
        return Math.abs(a - b) <= Math.ulp(b);
    }

    static void check(String name, DoubleUnaryOperator m, DoubleUnaryOperator strict) {
        StringBuilder sb = new StringBuilder(name);
        for (double x : SPECIAL) {
            double r = m.applyAsDouble(x);
            // Exact for the cases the specification fixes; otherwise within an ulp of fdlibm.
            sb.append(' ').append(Double.isNaN(r) || Double.isInfinite(r) || r == 0 || r == 1 ? String.valueOf(r) : within1ulp(r, strict.applyAsDouble(x)) ? "~" : "BAD" + r);
        }
        System.out.println(sb);
        java.util.Random rnd = new java.util.Random(7);
        double[] xs = new double[4096];
        for (int i = 0; i < xs.length; i++) xs[i] = (rnd.nextDouble() - 0.5) * (i % 3 == 0 ? 20 : i % 3 == 1 ? 2000 : 1e6);
        if (name.equals("log")) for (int i = 0; i < xs.length; i++) xs[i] = Math.abs(xs[i]);
        if (name.equals("exp")) for (int i = 0; i < xs.length; i++) xs[i] = xs[i] % 700;
        double[] cold = new double[xs.length];
        for (int i = 0; i < xs.length; i++) cold[i] = m.applyAsDouble(xs[i]);
        int far = 0, differ = 0;
        for (int round = 0; round < 40; round++) {
            for (int i = 0; i < xs.length; i++) {
                double r = m.applyAsDouble(xs[i]);
                if (round == 39) {
                    if (!within1ulp(r, strict.applyAsDouble(xs[i]))) far++;
                    if (Double.doubleToRawLongBits(r) != Double.doubleToRawLongBits(cold[i])) differ++;
                }
            }
        }
        System.out.println(name + ": beyond 1 ulp " + far + ", cold/hot differ " + differ);
    }

    public static void main(String[] a) {
        check("sin", MathSem::sin, StrictMath::sin);
        check("cos", MathSem::cos, StrictMath::cos);
        check("tan", MathSem::tan, StrictMath::tan);
        check("exp", MathSem::exp, StrictMath::exp);
        check("log", MathSem::log, StrictMath::log);
        System.out.println(Math.sin(-0.0) + " " + Math.tan(-0.0) + " " + Math.log(0.0) + " " + Math.log(-1.0) + " " + Math.exp(-0.0));
    }
}
