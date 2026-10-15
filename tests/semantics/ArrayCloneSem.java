import java.util.Arrays;

/** Array clone() compiled in place: every element type, empty and long arrays, a shallow copy
 *  of a two-level array, a subclass array seen through Object[], and independence of the copy. */
public class ArrayCloneSem {
    static int[] ci(int[] a) { return a.clone(); }
    static long[] cl(long[] a) { return a.clone(); }
    static byte[] cb(byte[] a) { return a.clone(); }
    static char[] cc(char[] a) { return a.clone(); }
    static boolean[] cz(boolean[] a) { return a.clone(); }
    static double[] cd(double[] a) { return a.clone(); }
    static float[] cf(float[] a) { return a.clone(); }
    static short[] cs(short[] a) { return a.clone(); }
    static Object[] co(Object[] a) { return a.clone(); }
    static int[][] cii(int[][] a) { return a.clone(); }
    static String[] cstr(String[] a) { return a.clone(); }

    public static void main(String[] args) {
        long h = 0;
        for (int round = 0; round < 20000; round++) {
            int n = round % 37;
            int[] ia = new int[n]; for (int i = 0; i < n; i++) ia[i] = i * round;
            int[] ib = ci(ia); if (ib == ia) throw new AssertionError("same int[]");
            if (n > 0) { ib[0] = -1; if (ia[0] == -1) throw new AssertionError("shared"); ib[0] = ia[0]; }
            h = h * 31 + Arrays.hashCode(ib) + ib.length;
            long[] la = new long[n]; for (int i = 0; i < n; i++) la[i] = (long) i << 33 | round;
            h = h * 31 + Arrays.hashCode(cl(la));
            byte[] ba = new byte[n]; for (int i = 0; i < n; i++) ba[i] = (byte) (i ^ round);
            h = h * 31 + Arrays.hashCode(cb(ba));
            char[] ca = new char[n]; for (int i = 0; i < n; i++) ca[i] = (char) ('a' + i);
            h = h * 31 + new String(cc(ca)).hashCode();
            boolean[] za = new boolean[n]; for (int i = 0; i < n; i++) za[i] = (i & round & 1) != 0;
            h = h * 31 + Arrays.hashCode(cz(za));
            double[] da = new double[n]; for (int i = 0; i < n; i++) da[i] = i * 0.5 + round;
            h = h * 31 + Arrays.hashCode(cd(da));
            float[] fa = new float[n]; for (int i = 0; i < n; i++) fa[i] = i * 0.25f;
            h = h * 31 + Arrays.hashCode(cf(fa));
            short[] sa = new short[n]; for (int i = 0; i < n; i++) sa[i] = (short) (i * 1000);
            h = h * 31 + Arrays.hashCode(cs(sa));
            String[] str = new String[n]; for (int i = 0; i < n; i++) str[i] = "s" + i;
            String[] str2 = cstr(str);
            Object[] viaObj = co(str);
            if (!(viaObj instanceof String[])) throw new AssertionError("lost class");
            h = h * 31 + Arrays.hashCode(str2) + viaObj.getClass().getName().hashCode();
            int[][] two = new int[n][]; for (int i = 0; i < n; i++) two[i] = ia;
            int[][] two2 = cii(two);
            if (n > 0 && two2[0] != ia) throw new AssertionError("deep");
            h = h * 31 + two2.length;
        }
        System.out.println(h);
        try {
            ci(null);
        } catch (NullPointerException e) {
            System.out.println("NPE");
        }
    }
}
