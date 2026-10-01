import java.util.HashMap;

/** Records' equals and hashCode, which javac compiles to invokedynamic on ObjectMethods: a
 *  method handle tree of getters and combinators that compiled code should reduce to plain
 *  field reads. Looks records up in a HashMap. */
public class RecordHashBench {
    record Key(Object fn, int kind, double scale) {}

    public static void main(String[] args) {
        Object[] fns = new Object[64];
        for (int i = 0; i < fns.length; i++) fns[i] = Integer.valueOf(i * 31);
        HashMap<Key, Integer> map = new HashMap<>();
        for (int i = 0; i < 4096; i++) map.put(new Key(fns[i & 63], i >> 6, i * 0.5), i);
        long check = 0;
        for (int round = 0; round < 10; round++) {
            long t0 = System.nanoTime();
            for (int k = 0; k < 400; k++) check += batch(map, fns);
            if (round >= 4) System.out.printf("%.1f ns/op%n", (System.nanoTime() - t0) / (400.0 * 4096));
        }
        System.out.printf("check %d%n", check);
    }

    static long batch(HashMap<Key, Integer> map, Object[] fns) {
        long s = 0;
        for (int i = 0; i < 4096; i++) {
            Integer v = map.get(new Key(fns[i & 63], i >> 6, i * 0.5));
            s += v == null ? -1 : v;
        }
        return s;
    }
}
