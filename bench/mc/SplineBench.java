import net.minecraft.util.CubicSpline;
import net.minecraft.util.ToFloatFunction;
/** Steady-state speed of CubicSpline.Multipoint.apply (the climate splines of world generation:
 *  a nested spline, binary search through a capturing lambda, Hermite interpolation), called
 *  from a hot method so both VMs compile the loop normally. Run with
 *  `-cp <classes>:<paper jar>:<libraries>`. */
public class SplineBench {
    static ToFloatFunction<float[]> coord(int k) {
        return new ToFloatFunction<float[]>() {
            public float apply(float[] p) { return p[k]; }
            public float minValue() { return -1f; }
            public float maxValue() { return 1f; }
        };
    }
    static double batch(CubicSpline<float[], ToFloatFunction<float[]>> s, float[] p, int k) {
        double sum = 0;
        for (int j = 0; j < 1000; j++) { int i = k * 1000 + j; p[0] = (i % 2000) / 1000f - 1f; p[1] = (i % 777) / 388f - 1f; sum += s.apply(p); }
        return sum;
    }

    public static void main(String[] a) {
        ToFloatFunction<float[]> x = coord(0), y = coord(1);
        CubicSpline<float[], ToFloatFunction<float[]>> inner = CubicSpline.<float[], ToFloatFunction<float[]>>builder(y)
            .addPoint(-1f, -0.5f, 0f).addPoint(-0.2f, 0.1f, 0.2f).addPoint(0.3f, 0.4f, 0f).addPoint(0.9f, 1f, 0f).build();
        CubicSpline<float[], ToFloatFunction<float[]>> s = CubicSpline.<float[], ToFloatFunction<float[]>>builder(x)
            .addPoint(-1f, inner).addPoint(-0.4f, 0.2f, 0f).addPoint(0f, inner).addPoint(0.5f, 0.7f, 0f).addPoint(1f, inner).build();
        float[] p = new float[2]; double sum = 0;
        for (int r = 0; r < 8; r++) {
            long t0 = System.nanoTime();
            for (int k = 0; k < 2000; k++) sum += batch(s, p, k);
            if (r >= 3) System.out.printf("%.1f ns/op%n", (System.nanoTime() - t0) / 2e6);
        }
        System.out.println("check " + sum);
    }
}
