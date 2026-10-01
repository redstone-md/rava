/** Optimized code at sites that deoptimize: two-dimensional `multianewarray` (negative counts
 *  included), receiver profiles that grow after compilation (the trapped site then calls),
 *  and constants first used after the method was compiled. */
public class OptTrapSem {
    interface Shape { int area(); }
    static final class Sq implements Shape { final int s; Sq(int s) { this.s = s; } public int area() { return s * s; } }
    static final class Rect implements Shape { final int w, h; Rect(int w, int h) { this.w = w; this.h = h; } public int area() { return w * h; } }
    static final class Tri implements Shape { final int b, h; Tri(int b, int h) { this.b = b; this.h = h; } public int area() { return b * h / 2; } }
    static final class Late { static int seed = 7; static int mix(int x) { return x * 31 + seed; } }

    static long grid(int r, int c) {
        double[][] g = new double[r][c];
        long s = g.length;
        for (int i = 0; i < r; i++) {
            s += g[i].length;
            g[i][c - 1] = i;
        }
        return s + (long) g[r - 1][c - 1];
    }

    static int total(Shape[] shapes, int n) {
        int t = 0;
        for (int i = 0; i < n; i++) t += shapes[i % shapes.length].area();
        return t;
    }

    static int maybeLate(int i) {
        return i < 200000 ? i & 7 : Late.mix(i);
    }

    public static void main(String[] args) {
        long s = 0;
        for (int i = 0; i < 20000; i++) s += grid(1 + (i & 7), 1 + (i % 5));
        System.out.println(s);
        try {
            grid(-1, 3);
        } catch (NegativeArraySizeException e) {
            System.out.println("negative " + e.getMessage());
        }
        try {
            Object[][] o = new Object[2][-5];
            System.out.println(o.length);
        } catch (NegativeArraySizeException e) {
            System.out.println("negative " + e.getMessage());
        }
        Shape[] one = {new Sq(3)};
        Shape[] many = {new Sq(3), new Rect(2, 5), new Tri(4, 6)};
        long t = 0;
        for (int i = 0; i < 3000; i++) t += total(i < 1500 ? one : many, 100);
        System.out.println(t);
        long u = 0;
        for (int i = 0; i < 300000; i++) u += maybeLate(i);
        System.out.println(u);
    }
}
