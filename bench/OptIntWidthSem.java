/** Int values whose register high half is not their sign extension: negative results of
 *  32-bit arithmetic reaching i2l, comparisons, array indexing, calls into natives and
 *  interpreted code, returns, and deoptimization. */
public class OptIntWidthSem {
    static int sink;

    static int neg(int a, int b) { return a - b; }
    static long widen(int a, int b) { int d = a - b; return d; }
    static long mixed(int a, int b) { int d = a * b - 7; return d + (long) d * 3L; }
    static boolean below(int a, int b) { int d = a - b; return d < 0; }
    static int pick(int[] arr, int a, int b) { int i = (a - b) & 7; return arr[i] + (a - b); }
    static int div(int a, int b) { int d = a - b; return d / 3 + d % 5; }
    static String text(int a, int b) { return Integer.toString(a - b) + ":" + Long.toString(a - b); }
    static int shift(int a, int b) { return ((a - b) >> 2) + ((a - b) >>> 28); }

    static long copy(int a, int b) {
        int[] src = {1, 2, 3, 4, 5, 6, 7, 8};
        int[] dst = new int[8];
        int n = b - a;
        try {
            System.arraycopy(src, a - b, dst, 0, 3);
        } catch (ArrayIndexOutOfBoundsException e) {
            n += 100;
        }
        System.arraycopy(src, 1, dst, 2, n & 3);
        long s = n;
        for (int x : dst) s += x;
        return s;
    }

    static int late(int a, int b) {
        int d = a - b;
        if (a == 123456) {
            // Rarely taken (deoptimizes with d live).
            return d * 2 + Integer.MIN_VALUE / (d | 1);
        }
        return d;
    }

    public static void main(String[] args) {
        int[] arr = {10, 20, 30, 40, 50, 60, 70, 80};
        long s = 0;
        for (int i = 0; i < 200000; i++) {
            int a = i % 1000, b = (i * 7) % 1300;
            s += neg(a, b) + widen(a, b) + mixed(a, b) + (below(a, b) ? 1 : 0) + pick(arr, a, b);
            s += div(a, b) + shift(a, b) + late(a, b);
            if ((i & 4095) == 0) {
                s += copy(a % 8, b % 8) + text(a, b).length();
            }
        }
        System.out.println(s);
        System.out.println(late(123456, 999999) + " " + widen(-5, 7) + " " + text(3, 9));
        System.out.println(Math.floorMod(neg(3, 11), 5) + " " + Math.abs(neg(3, 11)) + " " + Math.max(neg(1, 5), neg(2, 9)));
    }
}
