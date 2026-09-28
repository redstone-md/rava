// WHAT DO THE TOP SEAM CALLEES COST? — the JIT-callback census puts these five at 7.67% / 4.94% /
// 4.78% / 2.00% / 1.87% of a boot's 64,088,346 compiled-to-host crossings, and every one of them
// is a trivial operation: a type check, an iterator, a field read, two bit twiddles.
//
// ⚠Each loop in its own method, called twice, so it reaches the compiled tier by entry count.
import java.util.*;

public class SeamCost {
    static Object obj = "hello";
    static Class<String> cls = String.class;
    static Boolean box = Boolean.TRUE;
    static BitSet bits = new BitSet(4096);
    static it.unimi.dsi.fastutil.objects.Reference2ObjectArrayMap<Object, Object> rmap =
        new it.unimi.dsi.fastutil.objects.Reference2ObjectArrayMap<>();
    static Object[] rk = new Object[4];

    static long classCast(int n)  { long a = 0; for (int i = 0; i < n; i++) a += cls.cast(obj) == null ? 0 : 1; return a; }
    static long boolValue(int n)  { long a = 0; for (int i = 0; i < n; i++) a += box.booleanValue() ? 1 : 0; return a; }
    static long bitClear(int n)   { long a = 0; for (int i = 0; i < n; i++) { bits.clear(i & 4095); a += i & 1; } return a; }
    static long bitNext(int n)    { long a = 0; for (int i = 0; i < n; i++) a += bits.nextClearBit(i & 2047); return a; }
    static long mapIter(int n)    { long a = 0; for (int i = 0; i < n; i++) { for (Map.Entry<Object,Object> e : rmap.reference2ObjectEntrySet()) a += e.getKey() == null ? 0 : 1; } return a; }
    static long control(int n)    { long a = 0; for (int i = 0; i < n; i++) a += i & 1; return a; }

    static long time(String name, int which, int n, int reps) {
        long best = Long.MAX_VALUE, acc = 0;
        for (int r = 0; r < reps; r++) {
            long t0 = System.nanoTime();
            long v = switch (which) {
                case 0 -> classCast(n); case 1 -> boolValue(n); case 2 -> bitClear(n);
                case 3 -> bitNext(n);   case 4 -> mapIter(n);   default -> control(n);
            };
            long dt = System.nanoTime() - t0;
            acc += v; if (dt < best) best = dt;
        }
        System.out.println(name + " " + (best / n) + " ns/op");
        return acc;
    }

    public static void main(String[] args) {
        for (int i = 0; i < 4; i++) { rk[i] = new Object(); rmap.put(rk[i], rk[i]); }
        int n = 200_000, reps = 40;
        long acc = 0;
        acc += time("Class.cast        ", 0, n, reps);
        acc += time("Boolean.boolValue ", 1, n, reps);
        acc += time("BitSet.clear      ", 2, n, reps);
        acc += time("BitSet.nextClear  ", 3, n, reps);
        acc += time("R2OMap entry-iter ", 4, n / 4, reps);
        acc += time("control           ", 5, n, reps);
        System.out.println("acc=" + acc);
    }
}
