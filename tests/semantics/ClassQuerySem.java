// Class queries on classes known only at run time (isArray, isPrimitive, isInterface,
// isAssignableFrom), Reference.refersTo while referents are cleared, and array
// routines (Arrays.hashCode / equals) inside loops that change the arrays they read: hot
// enough to run as optimized code.
import java.lang.ref.PhantomReference;
import java.lang.ref.ReferenceQueue;
import java.lang.ref.WeakReference;
import java.util.Arrays;
import java.util.List;
import java.util.RandomAccess;

public class ClassQuerySem {
    static final Class<?>[] CLASSES = { int.class, void.class, int[].class, String[][].class, Object.class, String.class,
        List.class, RandomAccess.class, java.util.ArrayList.class, Integer.class, Number.class, Comparable.class, long.class };

    static int queries(int rounds) {
        int s = 0;
        for (int r = 0; r < rounds; r++) {
            for (Class<?> a : CLASSES) {
                s = s * 3 + (a.isArray() ? 1 : 0) + (a.isPrimitive() ? 2 : 0) + (a.isInterface() ? 4 : 0);
                for (Class<?> b : CLASSES) {
                    if (a.isAssignableFrom(b)) s += 7;
                }
            }
        }
        return s;
    }

    static int nullArgument(Class<?> c, Class<?> other) {
        try {
            return c.isAssignableFrom(other) ? 1 : 0;
        } catch (NullPointerException e) {
            return -1;
        }
    }

    static int refers(int rounds) {
        Object keep = new Object();
        WeakReference<Object> strong = new WeakReference<>(keep);
        PhantomReference<Object> phantom = new PhantomReference<>(keep, new ReferenceQueue<>());
        int s = 0;
        for (int r = 0; r < rounds; r++) {
            Object other = new int[r & 3];
            if (strong.refersTo(keep)) s++;
            if (strong.refersTo(other)) s += 100;
            if (phantom.refersTo(keep)) s += 2;
            if (phantom.refersTo(null)) s += 1000;
        }
        return s + (keep.hashCode() == System.identityHashCode(keep) ? 1 : 0);
    }

    /// A referent cleared while a loop keeps asking.
    static int cleared(int n) {
        WeakReference<Object> w = new WeakReference<>(new Object());
        int nulls = 0;
        for (int r = 0; r < n; r++) {
            if (w.refersTo(null)) nulls++;
            if (r == n / 2) w.clear();
        }
        return nulls;
    }

    static long hashes(int[] a, int n) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            a[i & 7] = i;
            s += Arrays.hashCode(a);
        }
        return s;
    }

    static int equal(byte[] a, byte[] b, int n) {
        int s = 0;
        for (int i = 0; i < n; i++) {
            a[i & 3] = (byte) (i & 1);
            if (Arrays.equals(a, b)) s++;
        }
        return s;
    }

    public static void main(String[] args) {
        int q = 0;
        for (int i = 0; i < 300; i++) q = queries(20);
        System.out.println("queries " + q);
        int npe = 0;
        for (int i = 0; i < 20000; i++) npe += nullArgument(Object.class, i == 19999 ? null : String.class);
        System.out.println("null " + npe + " " + nullArgument(int.class, int.class) + " " + nullArgument(int.class, Integer.class));
        int r = 0;
        for (int i = 0; i < 300; i++) r = refers(1000);
        System.out.println("refers " + r);
        int c = 0;
        for (int i = 0; i < 300; i++) c += cleared(1000);
        System.out.println("cleared " + c);
        long h = 0;
        int e = 0;
        for (int i = 0; i < 300; i++) {
            h += hashes(new int[8], 1000);
            e += equal(new byte[4], new byte[] { 0, 1, 0, 1 }, 1000);
        }
        System.out.println("arrays " + h + " " + e);
    }
}
