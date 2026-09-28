import net.minecraft.util.BoundedFloatFunction;
import net.minecraft.util.CubicSpline;
import java.util.ArrayList;
import java.util.List;

// Focused real-Minecraft spline fixture. Compile with:
// javac -cp "$(cat bench/mc-cp.txt)" -d /tmp bench/RavaSplineBench.java
// Run with `RAVA_JIT=1 RAVA_JIT_TIER0=1 target/release/rava -cp "$(cat bench/mc-cp.txt):/tmp" RavaSplineBench`.
// Add `RAVA_JIT_HOF_FUSION=1` to exercise structural compilation; add `RAVA_TRACE_COMPILE=1`
// to verify that Multipoint.findIntervalStart was compiled through its HOF plan.
public final class RavaSplineBench {
    static final class Point implements BoundedFloatFunction<Point> {
        final float value;
        Point(float value) { this.value = value; }
        public float apply(Point point) { return point.value; }
        public float minValue() { return 0.0f; }
        public float maxValue() { return 8192.0f; }
    }

    public static void main(String[] args) {
        float[] locations = new float[16];
        float[] derivatives = new float[16];
        List<CubicSpline<Point, Point>> values = new ArrayList<>(16);
        for (int i = 0; i < locations.length; i++) {
            locations[i] = i * 512.0f;
            values.add(CubicSpline.<Point, Point>constant(i));
        }
        Point coordinate = new Point(0.0f);
        CubicSpline<Point, Point> spline = new CubicSpline.Multipoint<>(
            coordinate, locations, values, derivatives, 0.0f, 15.0f);
        Point[] points = new Point[1024];
        for (int i = 0; i < points.length; i++) {
            points[i] = new Point((i * 127) & 8191);
        }

        for (int i = 0; i < 500_000; i++) spline.apply(points[i & 1023]);
        long sum = 0;
        long start = System.nanoTime();
        for (int i = 0; i < 2_000_000; i++) {
            sum += Float.floatToRawIntBits(spline.apply(points[i & 1023]));
        }
        System.out.println("SUM=" + sum + " HOTNS=" + (System.nanoTime() - start));
    }
}
