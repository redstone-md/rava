// System.arraycopy of references into a freshly allocated array: an exact class match copies
// inline, a subclass's array (String[] into Object[]) still copies, a store of the wrong kind
// (Object[] holding an Integer into String[]) throws ArrayStoreException, and bad ranges and
// null sources throw as specified. The copies survive collections.
import java.util.Arrays;

public class RefCopySem {
    static Object[] grow(Object[] a, int n) { Object[] b = new Object[n]; System.arraycopy(a, 0, b, 0, Math.min(a.length, n)); return b; }
    static String[] growS(String[] a, int n) { String[] b = new String[n]; System.arraycopy(a, 0, b, 0, Math.min(a.length, n)); return b; }
    static Object[] mid(Object[] a, int p, int n) { Object[] b = new Object[n + 2]; System.arraycopy(a, p, b, 1, n); return b; }
    static String[] toStrings(Object[] a) { String[] b = new String[a.length]; System.arraycopy(a, 0, b, 0, a.length); return b; }
    static <T> T[] copyOf(T[] a, int n) { return Arrays.copyOf(a, n); }

    public static void main(String[] args) {
        long acc = 0;
        Object[] src = new Object[40];
        String[] ss = new String[40];
        for (int i = 0; i < src.length; i++) { src[i] = "o" + i; ss[i] = "s" + i; }
        for (int r = 0; r < 60000; r++) {
            int n = 1 + r % 35;
            Object[] g = grow(src, n);
            String[] h = growS(ss, n + 3);
            Object[] m = mid(src, r % 5, n);
            Object[] c = copyOf(ss, n);
            Object[] w = grow(ss, n);
            acc += g[n - 1].hashCode() + h[n - 1].length() + (h[n + 2] == null ? 1 : 0) + m[n].hashCode() + c[n - 1].hashCode() + w.length + w[0].hashCode();
            if (r % 1000 == 0) { src[r % 40] = "n" + r; System.gc(); }
        }
        System.out.println("acc " + acc);
        try { mid(src, 39, 5); } catch (ArrayIndexOutOfBoundsException e) { System.out.println("aioobe src"); }
        try { mid(src, -1, 2); } catch (ArrayIndexOutOfBoundsException e) { System.out.println("aioobe neg"); }
        try { grow(null, 3); } catch (NullPointerException e) { System.out.println("npe"); }
        Object[] mixed = { "a", "b", 3, "d" };
        try { toStrings(mixed); } catch (ArrayStoreException e) { System.out.println("ase"); }
        System.out.println(Arrays.toString(toStrings(new Object[] { "x", "y" })));
        // Survives collection: references copied into an array that then lives long.
        Object[][] keep = new Object[100][];
        for (int i = 0; i < keep.length; i++) { keep[i] = grow(new Object[] { new StringBuilder("k" + i), i }, 2); if (i % 10 == 0) System.gc(); }
        for (int i = 0; i < 200000; i++) { Object[] junk = new Object[8]; junk[0] = junk; }
        System.gc();
        long t = 0;
        for (Object[] k : keep) t += k[0].toString().length() + (Integer) k[1];
        System.out.println("keep " + t);
    }
}
