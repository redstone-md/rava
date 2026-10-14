// Values live in registers across allocations (whose slow path collects and calls the runtime)
// and class casts (exact, at a primary depth, and one that fails and deoptimizes): a loop
// with more live values than registers, in a method without calls (whose frame is published
// only by a slow path), each value checked after the loop. The objects escape, so they are
// allocated.
public class LiveAcrossAllocSem {
    static class Base { long v; Base(long v) { this.v = v; } }
    static class Mid extends Base { Mid(long v) { super(v); } }
    static final class Leaf extends Mid { int pad; Leaf(long v) { super(v); } }
    static final class Big { long a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t; }

    static Object[] pool = new Object[64];
    static int retained;

    static long run(int n, Object[] xs) {
        long a = 1, b = 2, c = 3, d = 4, e = 5, f = 6, g = 7, h = 8, i2 = 9, j = 10, k = 11;
        for (int i = 0; i < n; i++) {
            Object o = xs[i & 15];
            Leaf leaf = new Leaf(a + i);
            pool[i & 63] = leaf;
            a += leaf.v ^ b;
            Base base = (Base) o;
            b += base.v + c;
            Mid mid = (Mid) xs[(i + 1) & 15];
            c += mid.v * 3 + d;
            Big big = new Big();
            pool[(i + 32) & 63] = big;
            big.t = e + f;
            d += big.t + big.a;
            e ^= f + (i & 7);
            f += g;
            g ^= h << 1;
            h += i2;
            i2 ^= j + i;
            j += k;
            k ^= a >>> 3;
            if ((i & 1023) == 0) pool[(retained++) & 63] = new long[512];
        }
        return a ^ b * 3 ^ c * 5 ^ d * 7 ^ e * 11 ^ f * 13 ^ g * 17 ^ h * 19 ^ i2 * 23 ^ j * 29 ^ k * 31;
    }

    public static void main(String[] args) {
        Object[] xs = new Object[16];
        for (int i = 0; i < 16; i++) xs[i] = i % 3 == 0 ? new Leaf(i) : new Mid(i * 7);
        long s = 0;
        for (int r = 0; r < 200; r++) s = s * 31 + run(20000, xs);
        System.out.println("sum " + s);
        // A cast that fails once the loop is compiled: a Base that is not a Mid.
        xs[5] = new Base(99);
        try {
            run(20000, xs);
            System.out.println("no exception");
        } catch (ClassCastException ex) {
            System.out.println("cast failed");
        }
        xs[5] = new Mid(5);
        System.out.println("again " + run(20000, xs));
    }
}
