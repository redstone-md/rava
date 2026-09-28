// fastutil IntOpenHashSet build churn — adds N ints one-by-one, repeated. rava
// models int-sets as a sorted-unique $elems array and re-drains+sorts+reallocs
// on every add → O(n^2 log n) build. The JVM's open-hash set is ~O(1)/add.
import it.unimi.dsi.fastutil.ints.IntOpenHashSet;
public class IntSetBuild {
    static long hot(int n, int reps) {
        long acc = 0;
        for (int r = 0; r < reps; r++) {
            IntOpenHashSet s = new IntOpenHashSet();
            for (int i = 0; i < n; i++) s.add(i ^ (r * 31));
            acc += s.size();
            for (int i = 0; i < n; i += 3) if (s.contains(i)) acc++;
        }
        return acc;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 8; w++) hot(500, 20);
        long t0 = System.nanoTime();
        long s = hot(3_000, 30);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
