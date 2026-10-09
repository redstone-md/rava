import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicReferenceArray;

// Parameters declared with a final class or an array of one have that class exactly when not
// null (calls on them and on their elements bind without a class test), and Unsafe accesses at
// a scaled array index fold into one address: null parameters and null elements, arrays read
// through an Object-typed callee, and CAS and stores at folded indexes. An inlined body's
// parameter says nothing beyond its guarded path: `Sq.area` passes its receiver on as a `Sq`,
// and the same receiver is a `Circ` on the other path.
public class DeclaredParamSem {
    static int hashOf(Object o) { return o == null ? -1 : o.hashCode(); }

    static long elements(String[] keys, int n) {
        long s = 0;
        for (int i = 0; i < n; i++) s += hashOf(keys[i % keys.length]);
        return s;
    }

    static int lengthOf(String s, Integer boxed) { return (s == null ? 0 : s.length()) + (boxed == null ? 0 : boxed); }

    static long table(ConcurrentHashMap<String, Integer> m, String[] keys, int n) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            String k = keys[i & 15];
            if (k == null) continue;
            Integer v = m.get(k);
            s += v == null ? 7 : v;
            if ((i & 63) == 0) m.merge(k, 1, Integer::sum);
            if ((i & 127) == 1) m.remove(k);
        }
        return s;
    }

    interface Shape { int area(); int tag(); }
    static final class Sq implements Shape {
        final int s;
        Sq(int s) { this.s = s; }
        public int area() { return twice(this); }
        public int tag() { return 1; }
        static int twice(Sq q) { return q.s * 2; }
    }
    static final class Circ implements Shape {
        final int r;
        Circ(int r) { this.r = r; }
        public int area() { return 3 * r * r; }
        public int tag() { return 2; }
    }

    static int shape(Shape x) { return x.area() * 10 + x.tag(); }

    static long atomics(AtomicReferenceArray<String> a, int n) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            int j = i & 7;
            String cur = a.get(j);
            if (!a.compareAndSet(j, cur, cur == null ? "x" + j : null)) s -= 1000;
            s += cur == null ? 1 : cur.length();
            if ((i & 31) == 0) a.lazySet(j, "z");
        }
        return s;
    }

    public static void main(String[] args) {
        String[] keys = new String[16];
        for (int i = 0; i < keys.length; i++) keys[i] = i % 5 == 3 ? null : "key" + i;
        ConcurrentHashMap<String, Integer> m = new ConcurrentHashMap<>();
        for (int i = 0; i < 12; i++) m.put("key" + i, i);
        AtomicReferenceArray<String> a = new AtomicReferenceArray<>(8);
        long acc = 0;
        for (int r = 0; r < 3000; r++) {
            acc += elements(keys, 64);
            acc += lengthOf(r % 4 == 0 ? null : "abc", r % 3 == 0 ? null : r);
            acc += table(m, keys, 256);
            acc += atomics(a, 64);
            acc += shape(r % 2 == 0 ? new Sq(r & 7) : new Circ(r & 3));
        }
        System.out.println("acc " + acc + " size " + m.size());
    }
}
