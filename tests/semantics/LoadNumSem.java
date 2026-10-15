import java.lang.reflect.Field;
import sun.misc.Unsafe;

// Field loads reused across branch merges (global load numbering): every way the field can
// change between two reads of it must be seen. A second reference to the same object, a store
// to a subclass's field at the same offset, a call, a volatile read, an Unsafe store into the
// object; while stores to unrelated classes' fields and off-heap memory must not matter.
public class LoadNumSem {
    static final Unsafe U;
    static final long X_OFF;
    static {
        try {
            Field f = Unsafe.class.getDeclaredField("theUnsafe");
            f.setAccessible(true);
            U = (Unsafe) f.get(null);
            X_OFF = U.objectFieldOffset(A.class.getDeclaredField("x"));
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }
    static class A { int x; int y; }
    static class B extends A { int z; }
    static final class C { int x; int y; }
    static volatile int vol;

    static void bump(A a) { a.x += 3; }

    static long run(A a, A alias, C c, int i, long mem) {
        long s = a.x;
        if ((i & 1) == 0) s += a.y; else s -= a.y;      // a merge
        alias.x += 1;                                     // the same object through another name
        s += a.x;
        if ((i & 2) == 0) c.x += 7; else c.y += 5;        // unrelated class, same offsets
        s += a.x * 3 + a.y;
        if ((i & 4) == 0) bump(a);                         // a call
        s += a.x;
        if ((i & 8) == 0) s += vol; else s -= 1;           // a volatile read
        s += a.y;
        U.putInt(a, X_OFF, a.x + 11);                      // Unsafe into the object
        s += a.x;
        U.putInt(null, mem + (i & 7) * 4L, (int) s);       // off-heap
        if ((i & 16) == 0) s ^= a.x; else s += a.y;
        if (a instanceof B b) { b.z += a.x; s += b.z; }
        return s;
    }

    public static void main(String[] args) {
        long mem = U.allocateMemory(64);
        A[] objs = {new A(), new B(), new A(), new B()};
        C c = new C();
        long s = 0;
        for (int r = 0; r < 300000; r++) {
            A a = objs[r & 3];
            A alias = (r & 8) == 0 ? a : objs[(r + 1) & 3];
            s = s * 31 + run(a, alias, c, r, mem);
        }
        System.out.println(s + " " + c.x + " " + c.y + " " + objs[1].x + " " + ((B) objs[3]).z);
        U.freeMemory(mem);
    }
}
