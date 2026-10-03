// instanceof and checkcast against interfaces from optimized code: inherited and
// super-interfaces, arrays (Cloneable, Serializable), misses, null and a failing cast.
import java.io.Serializable;
import java.util.*;

public class IfaceCheckSem {
    interface A {}
    interface B extends A {}
    interface C {}
    static class P implements B {}
    static class Q extends P implements C {}
    static class R {}

    static int probe(Object o) {
        int r = 0;
        if (o instanceof A) r |= 1;
        if (o instanceof B) r |= 2;
        if (o instanceof C) r |= 4;
        if (o instanceof Cloneable) r |= 8;
        if (o instanceof Serializable) r |= 16;
        if (o instanceof Comparable) r |= 32;
        if (o instanceof RandomAccess) r |= 64;
        return r;
    }

    static int cast(Object o) {
        try { C c = (C) o; return c == null ? 1 : 2; } catch (ClassCastException e) { return 3; }
    }

    public static void main(String[] args) {
        Object[] objs = { new P(), new Q(), new R(), new int[1], new String[1], "s", 7, new ArrayList<>(), new LinkedList<>(), null, new Q[0] };
        long acc = 0;
        for (int r = 0; r < 300000; r++) {
            Object o = objs[r % objs.length];
            acc = acc * 31 + probe(o) + cast(o);
        }
        System.out.println(acc);
        StringBuilder sb = new StringBuilder();
        for (Object o : objs) sb.append(probe(o)).append(':').append(cast(o)).append(' ');
        System.out.println(sb);
    }
}
