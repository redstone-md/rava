/** Array bounds checks the optimizing tier folds, merges or drops: constant indices of one
 *  array in a block (a failing later index must throw only after the earlier accesses ran),
 *  masked and shifted indices into constant and freshly allocated arrays, negative constant
 *  indices, and array lengths of arrays whose length is known. */
public class OptBoundsSem {
    static final int[][] GRAD = {{1, 1, 0}, {-1, 1, 0}, {1, -1, 0}, {-1, -1, 0}, {1, 0, 1}, {-1, 0, 1}, {1, 0, -1}, {-1, 0, -1},
        {0, 1, 1}, {0, -1, 1}, {0, 1, -1}, {0, -1, -1}, {1, 1, 0}, {0, -1, 1}, {-1, 1, 0}, {0, -1, -1}};
    static final byte[] PERM = new byte[256];
    static int sink;

    static double dot(int h, double x, double y, double z) {
        int[] g = GRAD[h & 15];
        return g[0] * x + g[1] * y + g[2] * z;
    }

    static int perm(int i) { return PERM[i & 255] & 0xff; }
    static int top(int i) { return PERM[i >>> 24]; }
    static int fresh(int i) { int[] a = new int[8]; a[i & 7] = i; return a[i & 7] + a.length; }

    /** Writes a[0], a[1] then reads a[2]: on a short array the first two stores must happen. */
    static int three(int[] a, int v) {
        a[0] = v;
        a[1] = v + 1;
        sink = a[1];
        return a[2] + a[0];
    }

    static int neg(int[] a, boolean bad) {
        int s = a[1];
        if (bad) s += a[-1];
        return s + a[0];
    }

    public static void main(String[] args) {
        for (int i = 0; i < 256; i++) PERM[i] = (byte) (i * 151 + 7);
        double acc = 0;
        long s = 0;
        int[] big = new int[4], small = new int[2];
        for (int i = 0; i < 300000; i++) {
            acc += dot(i * 31, i * 0.5, 1.25, -i);
            s += perm(i) + top(i * 977) + fresh(i) + three(big, i);
        }
        System.out.println(acc + " " + s + " " + big[0] + " " + big[1]);
        try {
            three(small, 77);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("short " + e.getMessage() + " " + small[0] + " " + small[1] + " " + sink);
        }
        long n = 0;
        for (int i = 0; i < 100000; i++) n += neg(big, false);
        try {
            neg(big, true);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("neg " + n + " " + e.getMessage());
        }
        System.out.println(GRAD.length + " " + PERM.length + " " + new int[5].length);
    }
}
