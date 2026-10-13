// Global value numbering: repeated pure arithmetic across blocks, branches and merges, int and
// long constants of the same value, commutative operands, float compares with NaN, and a
// deoptimization after values were merged.
public class GvnSem {
    interface Shape { int area(int k); }
    static final class Sq implements Shape { public int area(int k) { return k * k; } }
    static final class Tri implements Shape { public int area(int k) { return k * k / 2; } }

    static int twice(int x) { return x * 3 + 1; }

    static long mix(int a, int b, long c) {
        long s = twice(a) + twice(a);
        if ((a & 1) == 0) {
            s += a * b + (b * a) ^ (a + 1);
        } else {
            s -= (a + 1) * 7;
        }
        s += (a + 1) * 7;
        s += c + 1;
        s += (long) a + 1L;
        s += (a + 1L) ^ (a + 1);
        s += -a + -a;
        s += (long) (a << 3) + (long) (a << 3);
        return s;
    }

    static int floats(float x, float y) {
        int r = 0;
        if (x < y) r |= 1;
        if (Float.compare(x, y) < 0) r |= 2;
        if (x < y) r |= 4;
        if (!(x >= y)) r |= 8;
        return r + (int) (x * y) + (int) (y * x);
    }

    static int shapes(Shape s, int k) {
        int a = s.area(k + 1);
        int b = (k + 1) * (k + 1);
        return a + b + s.area(k + 1);
    }

    public static void main(String[] args) {
        long sum = 0;
        for (int i = 0; i < 30000; i++) {
            sum += mix(i, i ^ 5, i * 31L);
            sum += floats(i * 0.5f, (i % 7) * 1.5f);
            sum += floats(Float.NaN, i);
            sum += shapes(new Sq(), i % 100);
        }
        for (int i = 0; i < 2000; i++) {
            sum += shapes(i % 2 == 0 ? new Sq() : new Tri(), i % 50);
        }
        System.out.println("sum " + sum);
        System.out.println(mix(-7, 3, Long.MAX_VALUE) + " " + floats(-0.0f, 0.0f) + " " + floats(1f, Float.NaN));
    }
}
