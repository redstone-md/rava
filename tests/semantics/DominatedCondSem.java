// Conditions proved by dominating tests: a bounds check after an explicit range test, a null
// check of a value tested non-null (and of a cast of it), the same class test of an object
// again, and range tests implied by earlier ones; each still fails where it must.
public class DominatedCondSem {
    interface Shape { int area(); }
    static final class Sq implements Shape { final int s; Sq(int s) { this.s = s; } public int area() { return s * s; } }
    static final class Rect implements Shape { final int w, h; Rect(int w, int h) { this.w = w; this.h = h; } public int area() { return w * h; } }
    static final class Holder { Object o; Holder(Object o) { this.o = o; } }

    static int pick(int[] a, int i) {
        if (i < 0 || i >= a.length) return -1;
        return a[i] + (i > -5 ? 1 : 0) + (i >= 0 ? 2 : 0);
    }

    static int unchecked(int[] a, int i) {
        if (i < 0) return -2;
        return a[i];
    }

    static int sides(Holder h) {
        Object o = h.o;
        if (o == null) return 0;
        Shape s = (Shape) o;
        int r = s.area();
        if (o instanceof Sq q) r += q.s;
        if (s instanceof Sq q2) r += q2.s * 2;
        return r;
    }

    static int ranges(int x) {
        int r = 0;
        if (x > 10) {
            if (x > 5) r += 1;
            if (x < 3) r += 100;
            if (x != 7) r += 2;
            if (x == 11) r += 4;
        }
        if (x >= 0 && x < 4) {
            r += x == 2 ? 8 : 16;
            if (x <= 3) r += 32;
        }
        return r;
    }

    public static void main(String[] args) {
        int[] a = new int[16];
        for (int i = 0; i < a.length; i++) a[i] = i * 3;
        Holder[] hs = { new Holder(new Sq(3)), new Holder(new Rect(2, 5)), new Holder(null), new Holder(new Sq(4)) };
        long acc = 0;
        for (int r = 0; r < 50000; r++) {
            acc += pick(a, r % 20 - 2) + unchecked(a, r % 16) + sides(hs[r & 3]) + ranges(r % 15 - 2);
        }
        System.out.println("acc " + acc);
        try {
            System.out.println(unchecked(a, 16));
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("aioobe " + e.getMessage());
        }
        try {
            System.out.println(sides(new Holder("not a shape")));
        } catch (ClassCastException e) {
            System.out.println("cce");
        }
        System.out.println(pick(a, 15) + " " + pick(a, 16) + " " + pick(a, -1) + " " + ranges(11) + " " + ranges(2) + " " + ranges(0));
    }
}
