// Differential probe: Class, reflection and type semantics — what codecs and datafixers
// stand on. Pure JDK, seconds to run.
import java.util.*;
public class ClassSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    static String ex(Runnable r) {
        try { r.run(); return "NO THROW"; }
        catch (Throwable t) { return t.getClass().getName(); }
    }
    interface Shape { default String kind() { return "shape"; } static String tag() { return "S"; } }
    interface Named { String name(); }
    static class Base implements Shape { public String toString() { return "Base"; } }
    static class Derived extends Base implements Named { public String name() { return "d"; } }
    record Pair(int a, String b) {}
    enum Color { RED, GREEN }
    static int varargs(int... xs) { return xs.length; }
    static String overload(Object o) { return "obj"; }
    static String overload(String s) { return "str"; }
    public static void main(String[] a) {
        // --- names
        p("name", String.class.getName() + " " + int.class.getName() + " " + int[].class.getName()
            + " " + String[].class.getName() + " " + Pair.class.getName());
        p("simple", String.class.getSimpleName() + " " + int[].class.getSimpleName()
            + " " + Pair.class.getSimpleName() + " " + Color.class.getSimpleName());
        p("canonical", String.class.getCanonicalName() + " " + int[].class.getCanonicalName());
        p("toString", String.class.toString() + " " + int.class.toString());

        // --- identity and hierarchy
        p("same", (String.class == "x".getClass()) + " " + String.class.equals(String.class));
        p("super", Derived.class.getSuperclass().getSimpleName() + " " + Object.class.getSuperclass()
            + " " + int.class.getSuperclass());
        p("ifaces", Arrays.toString(Derived.class.getInterfaces()));
        p("assignable", Base.class.isAssignableFrom(Derived.class) + " " + Derived.class.isAssignableFrom(Base.class)
            + " " + Shape.class.isAssignableFrom(Derived.class));
        p("isInstance", Base.class.isInstance(new Derived()) + " " + Named.class.isInstance(new Base()));
        p("kindFlags", Color.class.isEnum() + " " + Pair.class.isRecord() + " " + int.class.isPrimitive()
            + " " + int[].class.isArray() + " " + Shape.class.isInterface());
        p("component", int[].class.getComponentType() + " " + String[][].class.getComponentType().getSimpleName());

        // --- enums and records through reflection
        p("enumConstants", Arrays.toString(Color.class.getEnumConstants()));
        p("recordComponents", Pair.class.getRecordComponents().length + " "
            + Pair.class.getRecordComponents()[0].getName() + " "
            + Pair.class.getRecordComponents()[1].getType().getSimpleName());

        // --- interfaces: default and static methods, diamond resolution
        p("default", new Base().kind() + " " + new Derived().kind() + " " + Shape.tag());
        p("dispatch", new Derived().toString() + " " + ((Base) new Derived()).toString());
        p("overload", overload("s") + " " + overload((Object) "s"));

        // --- casting and instanceof
        Object o = new Derived();
        p("instanceof", (o instanceof Base) + " " + (o instanceof Named) + " " + (o instanceof String));
        p("pattern", (o instanceof Base b2 ? b2.kind() : "-"));
        p("cast.ok", ((Base) o).kind());
        p("cast.bad", ex(() -> { Object x = "s"; Base b3 = (Base) x; }));
        p("classCast", ex(() -> Base.class.cast("s")));
        p("nullCast", Base.class.cast(null) == null);

        // --- arrays through reflection
        Object arr = java.lang.reflect.Array.newInstance(String.class, 2);
        java.lang.reflect.Array.set(arr, 0, "z");
        p("reflectArray", arr.getClass().getSimpleName() + " " + java.lang.reflect.Array.getLength(arr)
            + " " + java.lang.reflect.Array.get(arr, 0));
        p("arrayStore", ex(() -> { Object[] xs = new String[1]; xs[0] = 1; }));

        // --- varargs
        p("varargs", varargs() + " " + varargs(1) + " " + varargs(1, 2, 3));

        // --- boxing identity through Class
        p("boxTypes", Integer.class.isInstance(5) + " " + Object.class.isInstance(5)
            + " " + int.class.isInstance(5));
    }
}
