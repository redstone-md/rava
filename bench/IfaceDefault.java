// Interface-default-method compile probe: the exact PureTransformer.compute
// shape (default method = 3 invokeinterfaces incl a wide (D)D, dreturn).
// RingScan shows it compile_fail=1 + cached_fail=90938 -> the spline chain
// interprets forever. HOTNS ratio + RAVA_JIT_STATS on this bench isolate it.
public class IfaceDefault {
    interface Fn {
        Fn input();
        double transform(double v);
        default double compute(double ctx) {
            return input() == null ? ctx : transform(input().compute(ctx));
        }
    }

    static final class Leaf implements Fn {
        public Fn input() { return null; }
        public double transform(double v) { return v; }
        public double compute(double ctx) { return ctx * 1.0000001 + 0.5; }
    }

    static final class Wrap implements Fn {
        final Fn in;
        Wrap(Fn in) { this.in = in; }
        public Fn input() { return in; }
        public double transform(double v) { return v * 0.999999 + 0.25; }
    }

    static double hot(int iters, Fn f) {
        double acc = 0;
        for (int i = 0; i < iters; i++) acc += f.compute(acc % 3.0 - 1.0);
        return acc;
    }

    public static void main(String[] a) {
        Fn f = new Wrap(new Wrap(new Leaf()));
        for (int w = 0; w < 15; w++) hot(200_000, f);
        long t0 = System.nanoTime();
        double s = hot(5_000_000, f);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + Double.doubleToLongBits(s));
        System.out.println("HOTNS=" + ns);
    }
}
