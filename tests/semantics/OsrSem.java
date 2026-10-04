// Long loops in methods called once move into optimized code while they run (on-stack
// replacement): locals of every kind carry over, nested loops continue in the outer loop,
// speculation failing inside the loop and exceptions caught in the same method resume
// correctly, and float, long and reference results come back through the frame.
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

public class OsrSem {
    static final int N = 3_000_000;

    interface Shape { int area(); }
    static final class Sq implements Shape { final int s; Sq(int s) { this.s = s; } public int area() { return s * s; } }
    static final class Rect implements Shape { final int w, h; Rect(int w, int h) { this.w = w; this.h = h; } public int area() { return w * h; } }

    int field = 3;
    long total;

    static long kinds(int seed) {
        long l = seed;
        double d = 0.5;
        float f = 1.25f;
        int i = 0;
        String s = "x";
        for (int k = 0; k < N; k++) {
            l = l * 31 + k;
            d += k * 0.25;
            f = f * 0.5f + 1;
            i ^= k;
            if ((k & 0xfffff) == 0) s = s + k;
        }
        return l + (long) d + (long) f + i + s.hashCode();
    }

    static long nested(int rows) {
        long sum = 0;
        int[] row = new int[1000];
        for (int r = 0; r < rows; r++) {
            for (int c = 0; c < row.length; c++) {
                row[c] += r ^ c;
                sum += row[c];
            }
            sum ^= r;
        }
        return sum;
    }

    /// The profile sees one class for millions of iterations, then another.
    static long megamorphicLater(Shape[] shapes) {
        long a = 0;
        for (int k = 0; k < N; k++) {
            Shape s = shapes[k < N - 1000 ? 0 : k & 1];
            a += s.area();
        }
        return a;
    }

    static int caught() {
        int[] small = new int[10];
        int thrown = 0;
        for (int k = 0; k < N; k++) {
            try {
                small[k < N - 50 ? k % 10 : k] += 1;
            } catch (ArrayIndexOutOfBoundsException e) {
                thrown++;
            }
        }
        return thrown + small[3];
    }

    static double doubles() {
        double x = 1;
        for (int k = 1; k < N; k++) x += 1.0 / k;
        return x;
    }

    static float floats() {
        float x = 0;
        for (int k = 1; k < N; k++) x += 1.0f / (k & 1023 | 1);
        return x;
    }

    long instance() {
        for (int k = 0; k < N; k++) total += field + (k & 7);
        return total;
    }

    /// A slot holding a reference before the loop and an int inside it.
    static int reused(boolean flag) {
        int r = 0;
        {
            Object o = flag ? "a" : null;
            r += o == null ? 1 : 2;
        }
        {
            int j;
            for (j = 0; j < N; j++) r += j & 3;
        }
        return r;
    }

    static String collections() {
        Map<Integer, List<Integer>> m = new HashMap<>();
        Set<Integer> seen = new HashSet<>();
        for (int k = 0; k < 400_000; k++) {
            m.computeIfAbsent(k % 97, x -> new ArrayList<>()).add(k);
            if (!seen.add(k % 1013)) seen.remove(k % 7);
        }
        long s = 0;
        for (List<Integer> l : m.values()) s += l.size() * 7L + l.get(l.size() - 1);
        return s + "/" + seen.size();
    }

    static Object refResult() {
        Object last = null;
        for (int k = 0; k < N; k++) if ((k & 0x3ffff) == 0) last = Integer.valueOf(k);
        return last;
    }

    public static void main(String[] args) {
        System.out.println("kinds " + kinds(7));
        System.out.println("nested " + nested(4000));
        System.out.println("megamorphic " + megamorphicLater(new Shape[] { new Sq(3), new Rect(2, 5) }));
        System.out.println("caught " + caught());
        System.out.println("doubles " + doubles());
        System.out.println("floats " + floats());
        System.out.println("instance " + new OsrSem().instance());
        System.out.println("reused " + reused(true));
        System.out.println("collections " + collections());
        System.out.println("ref " + refResult());
    }
}
