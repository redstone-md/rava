// Generic fastutil double-list accessor churn — mirrors Minecraft's VoxelShape
// coordinate hot loop (DoubleArrayList/DoubleImmutableList getDouble + size, ~20%
// of block-bootstrap self-time). No jar specifics — pure third-party collection
// bytecode. Needs the fastutil jar on the classpath, so it runs standalone (not
// through bench/run.sh). A/B: `java -cp out:fastutil.jar DoubleList` vs
// `RAVA_JIT=1 RAVA_JIT_THRESHOLD=10 rava -cp out:fastutil.jar DoubleList` — SUM
// must match bit-exact; HOTNS is the routed-vs-callback ratio.
import it.unimi.dsi.fastutil.doubles.DoubleArrayList;

public class DoubleList {
    static double hot(DoubleArrayList a, int iters) {
        double acc = 0;
        int n = a.size();
        for (int i = 0; i < iters; i++)
            for (int j = 0; j < n; j++) acc += a.getDouble(j);
        return acc;
    }

    public static void main(String[] x) {
        DoubleArrayList a = new DoubleArrayList();
        for (int i = 0; i < 64; i++) a.add(i * 1.5);
        for (int w = 0; w < 20; w++) hot(a, 5000); // warm the compile threshold
        long t0 = System.nanoTime();
        double s = hot(a, 200000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + (long) s);
        System.out.println("HOTNS=" + ns);
    }
}
