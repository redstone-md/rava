// Mirrors Minecraft block-state dedup: a Map keyed by a fastutil
// Reference2ObjectArrayMap combo (Property->value). Each lookup forces rava's
// structural_key over the array-map key — the boot hotspot (Reference2Object-
// ArrayMap$EntrySet iteration). Values are small Integers so this isolates the
// map-iteration cost from any expensive key toString.
import it.unimi.dsi.fastutil.objects.Reference2ObjectArrayMap;
import java.util.HashMap;

public class MapKeyStress {
    static final Object P1 = new Object(), P2 = new Object(), P3 = new Object();

    static Reference2ObjectArrayMap<Object, Object> combo(int a, int b, int c) {
        Reference2ObjectArrayMap<Object, Object> m = new Reference2ObjectArrayMap<>();
        m.put(P1, a); m.put(P2, b); m.put(P3, c);
        return m;
    }

    static long hot(int iters) {
        HashMap<Reference2ObjectArrayMap<Object, Object>, Integer> dedup = new HashMap<>();
        for (int a = 0; a < 4; a++)
            for (int b = 0; b < 4; b++)
                for (int c = 0; c < 4; c++)
                    dedup.put(combo(a, b, c), a * 16 + b * 4 + c);
        long acc = 0;
        for (int i = 0; i < iters; i++) {
            int a = i & 3, b = (i >> 2) & 3, c = (i >> 4) & 3;
            Integer v = dedup.get(combo(a, b, c)); // structural_key over the array-map key
            if (v != null) acc += v;
        }
        return acc;
    }

    public static void main(String[] args) {
        int iters = args.length > 0 ? Integer.parseInt(args[0]) : 2000000;
        for (int w = 0; w < 5; w++) hot(iters / 10);
        long t0 = System.nanoTime();
        long acc = hot(iters);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + acc);
        System.out.println("HOTNS=" + ns);
    }
}
