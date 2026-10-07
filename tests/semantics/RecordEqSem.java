// Record equals and hashCode (method-handle trees from ObjectMethods) and a hand-built
// guardWithTest, hot enough to be inlined: null, another class, floating-point fields (NaN,
// -0.0), reference fields, nested records and arrays (compared by identity), and a guard whose
// outcome flips while the code runs.
import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;
import java.util.Objects;

public class RecordEqSem {
    record P(int x, long y, double d, String s) {}
    record Q(P p, int[] a) {}
    record R(int x) {}

    static boolean small(int x) { return x < 50; }
    static int yes(int x) { return x * 2; }
    static int no(int x) { return -x; }

    public static void main(String[] args) throws Throwable {
        int[] shared = { 1 };
        Object[] xs = {
            new P(1, 2, 3.5, "a"), new P(1, 2, 3.5, "a"), new P(1, 2, 3.5, "b"), new P(1, 2, Double.NaN, null),
            new P(1, 2, Double.NaN, null), new P(1, 2, -0.0, "a"), new P(1, 2, 0.0, "a"), null, new R(1), "a",
            new Q(new P(1, 2, 3.5, "a"), shared), new Q(new P(1, 2, 3.5, "a"), shared), new Q(new P(1, 2, 3.5, "a"), new int[] { 1 }),
        };
        long eq = 0, h = 0;
        for (int r = 0; r < 20000; r++) {
            for (int i = 0; i < xs.length; i++) {
                Object a = xs[i];
                if (a == null) continue;
                for (Object b : xs) {
                    if (a.equals(b)) eq += i + 1;
                    if (Objects.equals(b, a)) eq += 7;
                }
                if (r == 0 && !(a instanceof Q)) h += a.hashCode();
            }
        }
        MethodHandles.Lookup l = MethodHandles.lookup();
        MethodType it = MethodType.methodType(int.class, int.class);
        MethodHandle g = MethodHandles.guardWithTest(l.findStatic(RecordEqSem.class, "small", MethodType.methodType(boolean.class, int.class)),
            l.findStatic(RecordEqSem.class, "yes", it), l.findStatic(RecordEqSem.class, "no", it));
        long gs = 0;
        for (int i = 0; i < 200000; i++) gs += (int) g.invokeExact(i < 150000 ? i % 50 : i % 100);
        System.out.println(eq + " " + h + " " + gs);
    }
}
