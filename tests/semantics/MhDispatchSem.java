import java.lang.invoke.*;

/** Method handles of interface and virtual methods invoked hot on many receiver classes:
 *  overriding implementations, inherited and default methods, and a receiver class whose
 *  last-used interface changes between calls. */
public class MhDispatchSem {
    interface Shape { int sides(); default String kind() { return "shape" + sides(); } }
    interface Named { String name(); }
    static class Tri implements Shape, Named { public int sides() { return 3; } public String name() { return "tri"; } }
    static class Quad implements Shape, Named { public int sides() { return 4; } public String kind() { return "quad"; } public String name() { return "quad"; } }
    static class Square extends Quad { public String name() { return "square"; } }
    static class Base { int v() { return 1; } }
    static class Derived extends Base { int v() { return 2; } }

    public static void main(String[] args) throws Throwable {
        MethodHandles.Lookup l = MethodHandles.lookup();
        MethodHandle sides = l.findVirtual(Shape.class, "sides", MethodType.methodType(int.class));
        MethodHandle kind = l.findVirtual(Shape.class, "kind", MethodType.methodType(String.class));
        MethodHandle name = l.findVirtual(Named.class, "name", MethodType.methodType(String.class));
        MethodHandle v = l.findVirtual(Base.class, "v", MethodType.methodType(int.class));
        Object[] shapes = { new Tri(), new Quad(), new Square() };
        Base[] bases = { new Base(), new Derived() };
        long sum = 0;
        StringBuilder last = new StringBuilder();
        for (int i = 0; i < 300_000; i++) {
            Object s = shapes[i % 3];
            sum += (int) sides.invoke((Shape) s);
            String k = (String) kind.invoke((Shape) s);
            String n = (String) name.invoke((Named) s);
            sum += k.length() + n.length() + (int) v.invoke(bases[i & 1]);
            if (i >= 299_997) last.append(k).append('/').append(n).append(' ');
        }
        System.out.println(sum + " " + last);
    }
}
