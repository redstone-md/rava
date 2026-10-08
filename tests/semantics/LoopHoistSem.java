// Loop-invariant code motion through chains of objects: final fields read past stores to other
// fields at the same offset, null checks speculated on paths the loop may skip, and loads of
// an inlined receiver that must stay below the test of its class.
public class LoopHoistSem {
    static final class Leaf { final int x; Leaf(int x) { this.x = x; } }
    static final class Mid { final Leaf leaf; Mid(Leaf l) { leaf = l; } }
    static final class Top { final Mid mid; Top(Mid m) { mid = m; } }
    static final class Acc { long sum; }

    static long chain(Top t, Acc acc, int n) {
        for (int i = 0; i < n; i++) acc.sum += t.mid.leaf.x + i;
        return acc.sum;
    }

    // `t` may be null when `use` is false: the speculated check must not fail the loop.
    static long maybe(Top t, int[] a, boolean use) {
        long s = 0;
        for (int v : a) {
            if (use && v > 1000) s += t.mid.leaf.x;
            s += v;
        }
        return s;
    }

    interface Src { long get(); }
    static final class Wide implements Src { long a = 5; Mid m = new Mid(new Leaf(3)); public long get() { return a + m.leaf.x; } }
    static final class Narrow implements Src { public long get() { return 1; } }

    static long poly(Src s, int n) {
        long r = 0;
        for (int i = 0; i < n; i++) r += s.get();
        return r;
    }

    public static void main(String[] args) {
        Top t = new Top(new Mid(new Leaf(7)));
        long c = 0;
        for (int r = 0; r < 20000; r++) c += chain(t, new Acc(), 50);
        System.out.println("chain " + c);
        int[] a = new int[64];
        for (int i = 0; i < a.length; i++) a[i] = i * 31;
        long m = 0;
        for (int r = 0; r < 20000; r++) m += maybe(t, a, true);
        System.out.println("maybe " + m + " " + maybe(null, a, false) + " " + maybe(t, a, true));
        try {
            maybe(null, a, true);
        } catch (NullPointerException e) {
            System.out.println("npe");
        }
        long p = 0;
        for (int r = 0; r < 20000; r++) p += poly(new Wide(), 40);
        System.out.println("poly " + p + " " + poly(new Narrow(), 40) + " " + poly(new Wide(), 40));
    }
}
