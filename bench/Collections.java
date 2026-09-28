// Generic collection churn — HashMap + ArrayList put/get/iterate. Stresses
// object alloc, field access, virtual dispatch, hashing.
import java.util.*;
public class Collections {
    static long hot(int iters) {
        long acc = 0;
        HashMap<Integer,Integer> m = new HashMap<>();
        ArrayList<Integer> l = new ArrayList<>();
        for (int i = 0; i < iters; i++) {
            int k = i & 4095;
            Integer prev = m.put(k, i);
            if (prev != null) acc += prev;
            l.add(i);
            if ((i & 7) == 0) { acc += m.getOrDefault((i * 3) & 4095, 0); }
            if (l.size() > 1024) { acc += l.remove(0); }
        }
        for (Map.Entry<Integer,Integer> e : m.entrySet()) acc ^= e.getValue();
        return acc;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 15; w++) hot(20_000);             // warmup (autobox-heavy: rava's worst path)
        long t0 = System.nanoTime();
        long s = hot(150_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
