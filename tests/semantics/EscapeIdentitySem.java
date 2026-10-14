// Identity tests on objects that compiled code replaces by their fields: a fresh object equals
// itself only, never null, a parameter or an element read from memory; a phi merging it with
// another object keeps the test real; a record read through its generated getters (Unsafe reads
// at constant offsets) with a fresh key in a HashMap lookup.
import java.util.HashMap;

public class EscapeIdentitySem {
    static final class P { int x; P(int x) { this.x = x; } }
    record Key(Object a, int b, double c) {}
    static P shared = new P(-1);

    static int test(P other, P[] arr, int i, boolean pick) {
        P p = new P(i);
        int r = 0;
        if (p == other) r += 1;
        if (p != null) r += 2;
        if (p == arr[i & 3]) r += 4;
        if (p == p) r += 8;
        if (other == p) r += 16;
        P q = pick ? p : shared;
        if (q == p) r += 32;
        if (q == shared) r += 64;
        return r + p.x;
    }

    public static void main(String[] args) {
        P[] arr = {new P(1), new P(2), shared, null};
        long sum = 0;
        for (int i = 0; i < 300_000; i++) sum += test((i & 1) == 0 ? shared : null, arr, i, (i % 3) == 0);
        System.out.println(sum);
        Object[] objs = new Object[16];
        for (int i = 0; i < objs.length; i++) objs[i] = (i & 1) == 0 ? Integer.valueOf(i) : "s" + i;
        HashMap<Key, Integer> map = new HashMap<>();
        for (int i = 0; i < 1024; i++) map.put(new Key(objs[i & 15], i >> 4, i * 0.25), i);
        long hits = 0;
        for (int r = 0; r < 300; r++) {
            for (int i = 0; i < 1100; i++) {
                Integer v = map.get(new Key(objs[i & 15], i >> 4, i * 0.25));
                hits += v == null ? -1 : v;
            }
        }
        System.out.println(hits + " " + new Key("x", 1, Double.NaN).equals(new Key("x", 1, Double.NaN)));
    }
}
