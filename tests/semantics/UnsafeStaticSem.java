// sun.misc.Unsafe on static fields: the base is the class itself (as HotSpot's
// staticFieldBase), including code that passes the Class object directly, as mods do to read
// MethodHandles.Lookup.IMPL_LOOKUP. Interpreted and compiled accesses.
import java.lang.invoke.MethodHandles;
import java.lang.reflect.Field;
import sun.misc.Unsafe;

public class UnsafeStaticSem {
    static int counter = 7;
    static Object ref = "start";
    static long wide = 1L << 40;

    public static void main(String[] args) throws Exception {
        Field uf = Unsafe.class.getDeclaredField("theUnsafe");
        uf.setAccessible(true);
        Unsafe u = (Unsafe) uf.get(null);
        Field c = UnsafeStaticSem.class.getDeclaredField("counter");
        Field r = UnsafeStaticSem.class.getDeclaredField("ref");
        Field w = UnsafeStaticSem.class.getDeclaredField("wide");
        Object base = u.staticFieldBase(c);
        System.out.println("base is class " + (base == UnsafeStaticSem.class));
        long co = u.staticFieldOffset(c), ro = u.staticFieldOffset(r), wo = u.staticFieldOffset(w);
        System.out.println("int " + u.getInt(UnsafeStaticSem.class, co) + " ref " + u.getObject(UnsafeStaticSem.class, ro) + " long " + u.getLong(base, wo));
        u.putInt(UnsafeStaticSem.class, co, 42);
        u.putObject(UnsafeStaticSem.class, ro, "changed");
        System.out.println("after put " + counter + " " + ref);
        System.out.println("cas " + u.compareAndSwapObject(UnsafeStaticSem.class, ro, "changed", "swapped") + " " + ref + " " + u.compareAndSwapInt(base, co, 41, 0) + " " + counter);
        Field il = MethodHandles.Lookup.class.getDeclaredField("IMPL_LOOKUP");
        Object impl = u.getObject(MethodHandles.Lookup.class, u.staticFieldOffset(il));
        System.out.println("impl lookup " + (impl != null) + " " + (impl instanceof MethodHandles.Lookup));
        long sum = 0;
        for (int i = 0; i < 300_000; i++) {
            u.putInt(UnsafeStaticSem.class, co, i);
            sum += u.getInt(UnsafeStaticSem.class, co);
            Object o = u.getObject(UnsafeStaticSem.class, ro);
            if (o == null) sum = -1;
            if ((i & 1023) == 0) u.putObject(base, ro, "v" + (i & 3));
        }
        System.out.println("hot " + sum + " " + counter + " " + ref);
    }
}
