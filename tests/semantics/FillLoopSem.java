/** Fill loops (`for (i = from; i < to; i++) a[i] = x`) the optimizing tier turns into vector
 *  stores: every element type it takes, ranges from empty to past several vectors, starting
 *  at odd offsets, with neighbours that must stay untouched, and the counter read after. */
public class FillLoopSem {
    static int fillInt(int[] a, int from, int to, int x) {
        int i;
        for (i = from; i < to; i++) a[i] = x;
        return i;
    }
    static void fillLong(long[] a, int from, int to, long x) {
        for (int i = from; i < to; i++) a[i] = x;
    }
    static void fillFloat(float[] a, int from, int to, float x) {
        for (int i = from; i < to; i++) a[i] = x;
    }
    static void fillDouble(double[] a, int from, int to, double x) {
        for (int i = from; i < to; i++) a[i] = x;
    }
    static void whole(int[] a) {
        for (int i = 0; i < a.length; i++) a[i] = -1;
    }

    public static void main(String[] args) {
        long h = 0;
        for (int round = 0; round < 3000; round++) {
            for (int n = 0; n <= 37; n++) {
                int from = round % 5, to = Math.min(from + n, 40);
                int[] ia = new int[42];
                int end = fillInt(ia, from, to, round * 31 + n);
                long[] la = new long[42];
                fillLong(la, from, to, (long) round << 33 | n);
                float[] fa = new float[42];
                fillFloat(fa, from, to, round + n * 0.5f);
                double[] da = new double[42];
                fillDouble(da, from, to, round - n * 0.25);
                int[] wa = new int[n];
                whole(wa);
                for (int k = 0; k < 42; k++) h = h * 31 + ia[k] + la[k] + Float.floatToIntBits(fa[k]) + Double.doubleToLongBits(da[k]);
                for (int k = 0; k < n; k++) h = h * 7 + wa[k];
                h += end;
            }
        }
        System.out.println(h);
        try {
            fillInt(new int[4], 2, 9, 1);
            System.out.println("no exception");
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("AIOOBE " + e.getMessage());
        }
    }
}
