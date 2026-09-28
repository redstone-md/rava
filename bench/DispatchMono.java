// Monomorphic FINAL-class virtual dispatch — invokevirtual whose callee can't be
// overridden (final class), so the JIT inlines it with no receiver-class guard.
// Verifies + measures the final-effective virtual inline (Stage-2 A2-safe).
public class DispatchMono {
    static final class Vec3 {
        double x, y, z;
        Vec3(double x, double y, double z) { this.x = x; this.y = y; this.z = z; }
        double dot(Vec3 o) { return x * o.x + y * o.y + z * o.z; } // virtual, final class
        double norm2() { return x * x + y * y + z * z; }
    }

    static double hot(int iters, Vec3 a, Vec3 b) {
        double acc = 0;
        for (int i = 0; i < iters; i++) {
            acc += a.dot(b) + b.norm2();
        }
        return acc;
    }

    public static void main(String[] args) {
        Vec3 a = new Vec3(1.5, 2.0, -3.0);
        Vec3 b = new Vec3(0.5, -1.0, 4.0);
        for (int w = 0; w < 15; w++) hot(1_000_000, a, b); // warmup → compile
        long t0 = System.nanoTime();
        double s = hot(20_000_000, a, b);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + (long) s);
        System.out.println("HOTNS=" + ns);
    }
}
