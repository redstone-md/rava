// WHERE DOES AN ALLOCATION'S TIME GO? Four phases over identical loop skeletons,
// each adding exactly one construct, so the difference between two phases prices
// that construct and nothing else. Differential, not sampled: this session has
// already had five instruments report on a path they were not actually on.
public class AllocSplit {
    static final class Empty { }
    static final class Three { int x, y, z; Three(int x, int y, int z) { this.x = x; this.y = y; this.z = z; } }
    static final class ThreeF { int x, y, z; }

    // P0: the loop skeleton and the arithmetic, no object at all.
    static long p0(int n) { long a = 0; for (int i = 0; i < n; i++) a += i + i * 2 + i * 3; return a; }
    // P1: + one allocation of a fieldless object with the implicit ctor.
    static long p1(int n) { long a = 0; for (int i = 0; i < n; i++) { Empty e = new Empty(); a += i + i * 2 + i * 3 + (e == null ? 1 : 0); } return a; }
    // P2: + three int fields, written by a real <init> frame.
    static long p2(int n) { long a = 0; for (int i = 0; i < n; i++) { Three t = new Three(i, i * 2, i * 3); a += t.x + t.y + t.z; } return a; }
    // P3: same three fields, written INLINE — no <init> frame beyond Object's.
    static long p3(int n) { long a = 0; for (int i = 0; i < n; i++) { ThreeF t = new ThreeF(); t.x = i; t.y = i * 2; t.z = i * 3; a += t.x + t.y + t.z; } return a; }

    static long time(int which, int n) {
        long t0 = System.nanoTime(); long s;
        switch (which) { case 0: s = p0(n); break; case 1: s = p1(n); break; case 2: s = p2(n); break; default: s = p3(n); break; }
        long ns = System.nanoTime() - t0;
        System.out.println("P" + which + " SUM=" + s + " NS=" + ns + " NSPI=" + (ns / n));
        return ns;
    }
    public static void main(String[] a) {
        int n = a.length > 0 ? Integer.parseInt(a[0]) : 4_000_000;
        for (int w = 0; w < 4; w++) { p0(n / 20); p1(n / 20); p2(n / 20); p3(n / 20); }
        for (int w = 0; w < 4; w++) time(w, n);
    }
}
