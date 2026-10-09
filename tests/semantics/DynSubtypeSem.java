// Subtype tests against a class known only at run time (Class.isInstance / Class.cast with a
// varying mirror, and the store check of an array whose element class varies): classes at
// every depth (past the primary display's eight), interfaces (direct, inherited, via a
// superclass), arrays, and the negative answers, each asked repeatedly so cached answers are
// checked too.
import java.util.*;

public class DynSubtypeSem {
    interface I {} interface J extends I {} interface K {}
    static class A0 {} static class A1 extends A0 implements J {} static class A2 extends A1 {} static class A3 extends A2 {}
    static class A4 extends A3 {} static class A5 extends A4 implements K {} static class A6 extends A5 {} static class A7 extends A6 {}
    static class A8 extends A7 {} static class A9 extends A8 {} static class A10 extends A9 {}
    static class B extends A0 implements K {}

    static int test(Class<?> c, Object o) { return c.isInstance(o) ? 1 : 0; }
    static Object store(Object[] a, Object o) { a[0] = o; return a[0]; }

    public static void main(String[] args) {
        Class<?>[] classes = { Object.class, A0.class, A1.class, A2.class, A3.class, A4.class, A5.class, A6.class, A7.class, A8.class, A9.class, A10.class,
                               B.class, I.class, J.class, K.class, Runnable.class, Object[].class, A0[].class, A5[].class, I[].class, String.class, Comparable.class, CharSequence.class };
        Object[] objs = { new A0(), new A1(), new A2(), new A3(), new A4(), new A5(), new A6(), new A7(), new A8(), new A9(), new A10(), new B(),
                          new A5[1], new A10[2], new A1[0], new Object[1], "str", 7, null };
        StringBuilder table = new StringBuilder();
        long acc = 0;
        for (int r = 0; r < 3000; r++) {
            for (int ci = 0; ci < classes.length; ci++)
                for (int oi = 0; oi < objs.length; oi++) {
                    int t = test(classes[ci], objs[oi]);
                    acc = acc * 31 + t;
                    if (r == 0) table.append(t);
                }
        }
        System.out.println(table);
        System.out.println("acc " + acc);
        Object[][] arrays = { new A0[1], new A3[1], new A9[1], new I[1], new J[1], new K[1], new Object[1], new CharSequence[1] };
        StringBuilder st = new StringBuilder();
        for (int r = 0; r < 2000; r++)
            for (Object[] a : arrays)
                for (Object o : objs) {
                    if (o == null || o.getClass().isArray()) continue;
                    try { store(a, o); if (r == 0) st.append('+'); } catch (ArrayStoreException e) { if (r == 0) st.append('-'); }
                }
        System.out.println(st);
        try { A5.class.cast(new B()); } catch (ClassCastException e) { System.out.println("cce"); }
        System.out.println(K.class.cast(new A7()).getClass().getSimpleName());
    }
}
