import java.util.*;

/** Intrinsified float bit conversions and identity hashes: NaN payloads and signed zeros,
 *  hashes stable across calls, collections and threads and equal between Object.hashCode and
 *  System.identityHashCode, null, and call sites mixing classes that override hashCode with
 *  classes that do not. */
public class OptIntrinsicSem {
    record Point(double x, double y) {}
    static final class Plain {}
    static final class Custom { public int hashCode() { return 42; } }

    static long bits(double d) { return Double.doubleToRawLongBits(d); }
    static double back(long l) { return Double.longBitsToDouble(l); }
    static int fbits(float f) { return Float.floatToRawIntBits(f); }
    static float fback(int i) { return Float.intBitsToFloat(i); }
    static int hash(Object o) { return o.hashCode(); }

    public static void main(String[] args) throws Exception {
        double[] ds = {0.0, -0.0, 1.5, -2.25, Double.NaN, Double.longBitsToDouble(0x7ff8000000000123L), Double.MIN_VALUE, Double.MAX_VALUE, Double.NEGATIVE_INFINITY};
        float[] fs = {0f, -0f, 1.5f, -2.25f, Float.NaN, Float.intBitsToFloat(0x7fc00123), Float.MIN_VALUE, Float.NEGATIVE_INFINITY};
        long acc = 0;
        for (int i = 0; i < 200000; i++) {
            double d = ds[i % ds.length] * ((i & 1) == 0 ? 1 : -1);
            float f = fs[i % fs.length];
            acc += bits(d) ^ bits(back(bits(d) + 1)) ^ fbits(f) ^ fbits(fback(fbits(f) - 1));
            acc += Double.hashCode(d) + Float.hashCode(f) + new Point(d, i).hashCode();
        }
        System.out.println(acc);
        System.out.println(Long.toHexString(bits(ds[5])) + " " + Integer.toHexString(fbits(fs[5])) + " " + bits(-0.0) + " " + fbits(-0f));

        Object[] objs = new Object[64];
        for (int i = 0; i < objs.length; i++) objs[i] = (i % 3 == 0) ? new Custom() : (i % 3 == 1) ? new Plain() : "s" + i;
        int[] first = new int[objs.length];
        for (int i = 0; i < objs.length; i++) first[i] = hash(objs[i]);
        boolean same = true;
        for (int round = 0; round < 3000; round++) {
            for (int i = 0; i < objs.length; i++) {
                if (hash(objs[i]) != first[i]) same = false;
                if (!(objs[i] instanceof Custom) && !(objs[i] instanceof String) && System.identityHashCode(objs[i]) != first[i]) same = false;
            }
            if (round % 500 == 0) { byte[][] junk = new byte[2000][]; for (int j = 0; j < junk.length; j++) junk[j] = new byte[256]; }
        }
        final int[] seen = new int[1];
        Thread t = new Thread(() -> seen[0] = objs[1].hashCode());
        t.start(); t.join();
        System.out.println("stable " + same + " thread " + (seen[0] == first[1]) + " null " + System.identityHashCode(null) + " custom " + first[0]);
        Set<Object> set = new HashSet<>(Arrays.asList(objs));
        System.out.println("set " + set.size() + " " + set.contains(objs[1]) + " " + set.contains(new Plain()));
    }
}
