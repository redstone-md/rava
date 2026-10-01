/** If-converted branches (conditional moves): ternaries over ints, longs and references, both
 *  diamonds and triangles, comparisons against large constants and between references, nested
 *  ternaries, arms with shifts and conversions, and values compared at the int and long edges. */
public class OptSelectSem {
    static long distance(long target, long min, long max) {
        long l = target - max, m = min - target;
        return l > 0L ? l : Math.max(m, 0L);
    }
    static int clampShift(int x, int s) { return x < 0 ? -x >> 1 : x << (s & 7); }
    static long wide(long x) { return x > 0x1_0000_0000L ? x - 0x1_0000_0000L : (int) x; }
    static Object pick(Object a, Object b, int k) { return (k & 1) == 0 ? a : b; }
    static boolean same(Object a, Object b) { return a == b; }
    static int nested(int a, int b, int c) { return a < b ? (b < c ? c : b) : (a < c ? c : a); }
    static int triangle(int x) { int r = x; if (x > 100) r = x - 100; return r; }
    static long edges(long x) { return x == Long.MIN_VALUE ? 1 : x == Long.MAX_VALUE ? 2 : x < 0 ? 3 : 4; }

    public static void main(String[] args) {
        long s = 0;
        Object p = new Object(), q = "q";
        long[] specials = {0, 1, -1, Long.MIN_VALUE, Long.MAX_VALUE, 0x1_0000_0000L, 0x1_0000_0001L, Integer.MIN_VALUE, Integer.MAX_VALUE};
        for (int i = 0; i < 400000; i++) {
            long t = (i * 2654435761L) % 4000 - 2000;
            s += distance(t, -500, 700) + clampShift((int) t, i) + wide(t * 1_000_003L);
            s += pick(p, q, i) == q ? 7 : 3;
            s += same(pick(p, q, i), p) ? 1 : 0;
            s += nested((int) t, i % 97, i % 1013) + triangle((int) t) + edges(specials[i % specials.length] + (i & 1));
        }
        System.out.println(s);
    }
}
