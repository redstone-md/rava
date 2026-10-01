import java.util.function.IntPredicate;

/** Final fields of objects allocated in the same compiled method read back what the
 *  constructor stored: branches assigning a final field differently, a superclass constructor
 *  reading a subclass field (through a virtual call) before it is assigned, records, capturing
 *  lambdas, and final arrays whose elements change. */
public class OptFinalFieldSem {
    static class Base {
        final int seen;
        Base() { seen = peek(); }
        int peek() { return -1; }
    }
    static final class Sub extends Base {
        final int v;
        Sub(int x) { super(); v = x * 3; }
        @Override int peek() { return v; }
    }
    static final class Branchy {
        final long a;
        final double d;
        Branchy(int x) {
            if ((x & 1) == 0) { a = x * 7L; d = x / 2.0; } else { a = -x; d = -1.5; }
        }
    }
    record Pair(int left, Object right) {}
    static final class Holder {
        final int[] cells;
        Holder(int n) { cells = new int[n]; }
    }

    static int search(float[] locations, float f) {
        IntPredicate p = i -> f < locations[i];
        int lo = 0, n = locations.length;
        while (n > 0) {
            int half = n / 2, mid = lo + half;
            if (p.test(mid)) n = half; else { lo = mid + 1; n -= half + 1; }
        }
        return lo;
    }

    static long run(int x, float[] locs) {
        Sub s = new Sub(x);
        Branchy b = new Branchy(x);
        Pair p = new Pair(x, s);
        Holder h = new Holder(4);
        long acc = s.seen + s.v + b.a + (long) (b.d * 4) + p.left() + (p.right() == s ? 1 : 0);
        for (int i = 0; i < 4; i++) {
            h.cells[i] += x + i;
            acc += h.cells[i] + search(locs, (x % 50) / 10f - 2.5f);
        }
        return acc;
    }

    public static void main(String[] args) {
        float[] locs = {-2f, -1f, -0.5f, 0f, 0.25f, 1f, 2f};
        long sum = 0;
        for (int x = 0; x < 300000; x++) sum += run(x, locs);
        System.out.println(sum);
    }
}
