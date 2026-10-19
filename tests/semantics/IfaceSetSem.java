import java.io.Serializable;

// Interface type checks in compiled code: classes with many interfaces, interfaces inherited
// through superinterfaces and superclasses, arrays, and failing casts.
public class IfaceSetSem {
    interface A {}
    interface B extends A {}
    interface C {}
    interface D extends B, C {}
    interface E1 {} interface E2 {} interface E3 {} interface E4 {} interface E5 {}
    interface E6 {} interface E7 {} interface E8 {} interface E9 {} interface E10 {}
    interface Never {}
    static class Many implements E1, E2, E3, E4, E5, E6, E7, E8, E9, E10, D {}
    static class Sub extends Many implements Serializable {}
    static class Plain {}

    static int score(Object o) {
        int s = 0;
        if (o instanceof A) s += 1;
        if (o instanceof C) s += 2;
        if (o instanceof E7) s += 4;
        if (o instanceof Never) s += 8;
        if (o instanceof Serializable) s += 16;
        if (o instanceof Cloneable) s += 32;
        return s;
    }

    static int cast(Object o) {
        try {
            E10 e = (E10) o;
            return e == null ? 1 : 2;
        } catch (ClassCastException ex) {
            return 3;
        }
    }

    public static void main(String[] args) {
        Object[] os = { new Many(), new Sub(), new Plain(), "s", new int[1], new Many[1], null, new Object() };
        long sum = 0;
        int[] last = new int[os.length];
        for (int i = 0; i < 3_000_000; i++) {
            int k = i % os.length;
            int s = score(os[k]) * 4 + cast(os[k]);
            last[k] = s;
            sum += s;
        }
        System.out.println("sum=" + sum);
        for (int k = 0; k < os.length; k++) System.out.println(k + " " + last[k]);
    }
}
