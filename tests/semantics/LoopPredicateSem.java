// Loop-invariant null checks predicated before the loop, and nested conditionals turned into
// conditional moves, must keep Java semantics: an NPE inside the loop comes after the side
// effects of earlier iterations and statements, and every arm of a nested ternary is right.
public class LoopPredicateSem {
    static final class Box { final long[] v; final int k; Box(long[] v, int k) { this.v = v; this.k = k; } }
    static int touched;

    static long sum(Box b, int n) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            touched++;
            s += b.v[i % b.v.length] * b.k;
        }
        return s;
    }

    static long dist(long[] p, long[] lo, long[] hi) {
        long d = 0;
        for (int i = 0; i < p.length; i++) {
            long a = p[i] - hi[i], b = lo[i] - p[i];
            d += a > 0 ? a * a : b > 0 ? b * b : 0;
        }
        return d;
    }

    static int pick(long x, long y) { return x > y ? (x > 2 * y ? 3 : 2) : (x == y ? 1 : 0); }

    public static void main(String[] args) {
        Box box = new Box(new long[] {1, 2, 3}, 5);
        long acc = 0;
        for (int r = 0; r < 20000; r++) acc += sum(box, 7);
        System.out.println("sum " + acc + " touched " + touched);
        for (int r = 0; r < 3; r++) {
            touched = 0;
            try { sum(null, 4); } catch (NullPointerException e) { System.out.println("npe after " + touched); }
            touched = 0;
            System.out.println("empty " + sum(null, 0) + " touched " + touched);
        }
        long[] lo = {0, 10, 20, -5}, hi = {5, 15, 25, 5};
        long t = 0;
        java.util.Random rnd = new java.util.Random(7);
        for (int r = 0; r < 200000; r++) {
            long[] p = {rnd.nextInt(40) - 10, rnd.nextInt(40) - 10, rnd.nextInt(40) - 10, rnd.nextInt(40) - 10};
            t += dist(p, lo, hi) + pick(p[0], p[1]) + pick(p[2], p[2]);
        }
        System.out.println("dist " + t);
    }
}
