/** Subtype checks through the primary supers display: a hierarchy deeper than the display,
 *  instanceof true and false at polymorphic sites, failing casts, arrays against classes, and
 *  interfaces (which take the other path). */
public class OptSubtypeSem {
    static class A0 {}
    static class A1 extends A0 {}
    static class A2 extends A1 {}
    static class A3 extends A2 {}
    static class A4 extends A3 {}
    static class A5 extends A4 {}
    static class A6 extends A5 {}
    static class A7 extends A6 {}
    static class A8 extends A7 {}
    static class A9 extends A8 implements Runnable { public void run() {} }
    static class B1 extends A0 {}

    static int classify(Object o) {
        int r = 0;
        if (o instanceof A0) r |= 1;
        if (o instanceof A3) r |= 2;
        if (o instanceof A6) r |= 4;
        if (o instanceof A8) r |= 8;
        if (o instanceof A9) r |= 16;
        if (o instanceof B1) r |= 32;
        if (o instanceof Runnable) r |= 64;
        if (o instanceof Object[]) r |= 128;
        if (o instanceof Number) r |= 256;
        return r;
    }

    static int cast(Object o) {
        try {
            A7 a = (A7) o;
            return a == null ? 1 : 2;
        } catch (ClassCastException e) {
            return 3;
        }
    }

    public static void main(String[] args) {
        Object[] objs = {new A0(), new A1(), new A3(), new A5(), new A7(), new A8(), new A9(), new B1(), "s", 7, new A2[1], new Object[2], null};
        long sum = 0;
        int[] casts = new int[4];
        for (int i = 0; i < 300000; i++) {
            Object o = objs[i % objs.length];
            sum += classify(o) * (i % 13 + 1);
            casts[cast(o)]++;
        }
        System.out.println(sum + " " + casts[1] + " " + casts[2] + " " + casts[3]);
        StringBuilder sb = new StringBuilder();
        for (Object o : objs) sb.append(classify(o)).append(',').append(cast(o)).append(' ');
        System.out.println(sb);
    }
}
