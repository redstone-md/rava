// Branches on booleans merged from inlined methods (each path returning a constant or a
// computed value), in loops, nested, negated, with a value merged alongside and with a path
// that only runs late (deoptimization through the merged branch): jump threading must keep
// every path's result and the values that flow past the branch.
import java.util.ArrayList;
import java.util.List;
import java.util.Objects;

public class BoolMergeSem {
    static boolean small(int x) {
        if (x < 0) return false;
        if (x < 10) return true;
        return x % 7 == 3;
    }

    static boolean same(Object a, Object b) {
        if (a == b) return true;
        if (a == null || b == null) return false;
        return a.equals(b);
    }

    static int countSmall(int n) {
        int c = 0;
        for (int i = -5; i < n; i++) {
            if (small(i)) c += 2; else c -= 1;
            if (!small(i ^ 5)) c ^= i;
        }
        return c;
    }

    static int merged(int n) {
        int s = 0;
        for (int i = 0; i < n; i++) {
            int extra = (i & 1) == 0 ? i : -i;
            boolean ok = (i & 3) == 0 ? true : (i & 3) == 1 ? false : small(i);
            if (ok) s += extra; else s -= extra >> 1;
        }
        return s;
    }

    static int equalsLoop(List<Object> xs, Object probe) {
        int hits = 0;
        for (Object x : xs) {
            if (same(x, probe)) hits++;
            if (Objects.equals(x, probe) != same(probe, x)) hits += 1000;
        }
        return hits;
    }

    /// The false path of the merged branch never runs while the method warms up.
    static long late(int n, int flip) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            boolean b = i < flip ? true : small(i);
            if (b) s += i; else s -= 3;
        }
        return s;
    }

    public static void main(String[] args) {
        long a = 0, b = 0, d = 0;
        for (int r = 0; r < 400; r++) {
            a += countSmall(500);
            b += merged(700);
            d += late(1000, r < 300 ? 1000 : 400);
        }
        List<Object> xs = new ArrayList<>();
        for (int i = 0; i < 50; i++) xs.add(i % 3 == 0 ? null : i % 3 == 1 ? "s" + (i % 5) : Integer.valueOf(i % 4));
        int h = 0;
        for (int r = 0; r < 2000; r++) h += equalsLoop(xs, r % 3 == 0 ? null : r % 3 == 1 ? "s2" : Integer.valueOf(2));
        System.out.println(a + " " + b + " " + d + " " + h);
    }
}
