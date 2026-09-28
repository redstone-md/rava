// Generic fastutil int-list + ref-map churn. Mirrors Minecraft codec/shape
// hot loops: IntImmutableList iteration + getInt + Reference2ObjectArrayMap
// lookups. No jar specifics — pure third-party collection bytecode.
import it.unimi.dsi.fastutil.ints.IntImmutableList;
import it.unimi.dsi.fastutil.ints.IntIterator;
import it.unimi.dsi.fastutil.ints.IntList;
import it.unimi.dsi.fastutil.objects.Reference2ObjectArrayMap;

public class FastUtil {
    static final Object KA = new Object(), KB = new Object(), KC = new Object();

    static long hot(int iters) {
        long acc = 0;
        IntList list = IntImmutableList.of(3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3);
        Reference2ObjectArrayMap<Object,Integer> m = new Reference2ObjectArrayMap<>();
        m.put(KA, 11); m.put(KB, 22); m.put(KC, 33);
        for (int i = 0; i < iters; i++) {
            // enhanced for-each over IntImmutableList → iterator()/hasNext/nextInt
            for (IntIterator it = list.iterator(); it.hasNext(); ) {
                acc += it.nextInt();
            }
            // indexed getInt + size
            int n = list.size();
            for (int j = 0; j < n; j++) acc ^= list.getInt(j);
            // ref-map get
            Integer v = m.get((i & 1) == 0 ? KA : KC);
            if (v != null) acc += v;
            if (m.containsKey(KB)) acc += 1;
        }
        return acc;
    }

    public static void main(String[] a) {
        for (int w = 0; w < 15; w++) hot(20_000);
        long t0 = System.nanoTime();
        long s = hot(150_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
