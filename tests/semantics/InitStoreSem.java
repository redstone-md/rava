// New objects whose fields the constructor stores right away (uncleared memory then), reads
// before storing (must read zero), stores only partly (a byte of a word), or stores after a
// check that may leave compiled code; and objects reused after a collection.
public class InitStoreSem {
    static final class P { final double x, y, z; P(double x, double y, double z) { this.x = x; this.y = y; this.z = z; } }
    static final class Q { int a; int b; Q(int v) { this.b = this.a + v; this.a = v * 2; } }
    static final class R { byte f; short g; int h; long k; R(int v) { f = (byte) v; k = v * 3L; } }
    static final class S { Object o; int n; S(Object o, int n) { this.n = n; if (n == 777777) throw new IllegalStateException(); this.o = o; } }

    public static void main(String[] args) {
        long h = 0;
        Object keep = null;
        for (int i = 0; i < 1_000_000; i++) {
            P p = new P(i, i * 0.5, -i);
            Q q = new Q(i);
            R r = new R(i);
            S s;
            try { s = new S(p, i); } catch (IllegalStateException e) { s = new S(null, -1); }
            h = h * 31 + (long) (p.x + p.y + p.z) + q.a + q.b + r.f + r.g + r.h + r.k + s.n + (s.o == null ? 7 : 1);
            if ((i & 4095) == 0) keep = new long[2000];
        }
        System.out.println(h + " " + (keep != null));
    }
}
