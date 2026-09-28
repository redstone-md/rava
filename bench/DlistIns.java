// NoiseParameters amplitude-corruption repro (no bootstrap): vanilla builds
// DoubleArrayList(rest) then add(0, first) — rava lost rest[1]=1.0 (parity dump
// showed [1.5,0,0,0,0,0] vs JVM [1.5,0,1,0,0,0]). Cold + JIT-warm checks:
// post-2d6c29e *List* classes compile, so the insert path may only break warm.
import it.unimi.dsi.fastutil.doubles.DoubleArrayList;

public class DlistIns {
    public static void main(String[] args) {
        System.out.println("COLD=" + build(1.5, new double[]{0.0, 1.0, 0.0, 0.0, 0.0}));
        double sink = 0;
        for (int i = 0; i < 300_000; i++) {
            DoubleArrayList l = build(1.5, new double[]{0.0, 1.0, 0.0, 0.0, 0.0});
            sink += l.getDouble(2) + l.getDouble(0);
        }
        System.out.println("SINK=" + sink);
        System.out.println("WARM=" + build(1.5, new double[]{0.0, 1.0, 0.0, 0.0, 0.0}));
        DoubleArrayList w = build(1.5, new double[]{0.0, 1.0, 0.0, 0.0, 0.0});
        StringBuilder sb = new StringBuilder("ELEMS=");
        for (int i = 0; i < w.size(); i++) sb.append(w.getDouble(i)).append(',');
        System.out.println(sb);
    }

    static DoubleArrayList build(double first, double[] rest) {
        DoubleArrayList l = new DoubleArrayList(rest);
        l.add(0, first);
        return l;
    }
}
