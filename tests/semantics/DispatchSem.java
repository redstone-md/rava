// Differential probe: inheritance and dispatch — super calls, interface defaults and
// their diamonds, field hiding, static vs instance, initialisation order. Core VM
// semantics: a divergence here is not a missing method, it is a wrong program.
public class DispatchSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }

    interface A { default String who() { return "A"; } }
    interface B extends A { default String who() { return "B"; } }
    interface C extends A { }
    static class D implements B, C { }                       // B wins: most specific
    static class E implements B, C { public String who() { return "E/" + B.super.who(); } }

    static class Base {
        static String sTag() { return "base-static"; }
        String tag() { return "base"; }
        String callTag() { return "via:" + tag(); }           // virtual from a base method
        String field = "base-field";
        Base() { init(); }                                    // calls an overridden method from a ctor
        void init() { }
    }
    static class Sub extends Base {
        static String sTag() { return "sub-static"; }
        String field = "sub-field";                           // HIDES, does not override
        int ctorSaw = -1;
        int ready = 7;
        @Override String tag() { return "sub"; }
        @Override void init() { ctorSaw = ready; }             // ready is still 0 here
        String superTag() { return super.tag(); }
        String bothFields() { return field + "/" + ((Base) this).field; }
    }

    static class Counter {
        static int made;
        static { made = 100; }
        Counter() { made++; }
    }

    static class Outer {
        private int x = 5;
        class Inner { int seeOuter() { return x; } }
        static class Nested { int val() { return 9; } }
        Runnable capture() { int local = 3; return () -> p("lambda.capture", x + local); }
    }

    public static void main(String[] a) {
        // --- virtual dispatch and super
        Base b = new Sub();
        p("virtual", b.tag() + " " + b.callTag());
        p("super", new Sub().superTag());
        p("static.hiding", Base.sTag() + " " + Sub.sTag());

        // --- field hiding is by STATIC type, not by object
        Sub s = new Sub();
        p("field.static", s.field + " " + ((Base) s).field + " " + s.bothFields());

        // --- a constructor calling an overridden method sees uninitialised subclass state
        p("ctor.order", s.ctorSaw + " " + s.ready);

        // --- interface defaults and the diamond
        p("diamond", new D().who());
        p("iface.super", new E().who());
        p("iface.ref", ((A) new D()).who() + " " + ((B) new D()).who());

        // --- static init order
        p("clinit", Counter.made + " " + newCounters(3) + " " + Counter.made);

        // --- inner and nested classes
        Outer o = new Outer();
        p("inner", o.new Inner().seeOuter() + " " + new Outer.Nested().val());
        o.capture().run();

        // --- instanceof through the hierarchy, and getClass identity
        p("types", (b instanceof Sub) + " " + (b instanceof Base) + " " + b.getClass().getSimpleName()
            + " " + (b.getClass() == Sub.class));

        // --- anonymous class and its capture
        int captured = 11;
        Base anon = new Base() { @Override String tag() { return "anon" + captured; } };
        p("anon", anon.tag() + " " + anon.callTag() + " " + anon.getClass().getSuperclass().getSimpleName());

        // --- arrays are covariant, and the store check is dynamic
        Object[] objs = new String[1];
        p("covariance", objs.getClass().getSimpleName() + " " + (objs instanceof String[]));

        // --- string switch and fallthrough
        p("switch", sw("a") + sw("b") + sw("z"));
    }
    static int newCounters(int n) { for (int i = 0; i < n; i++) new Counter(); return n; }
    static String sw(String k) {
        switch (k) {
            case "a": return "A";
            case "b":
            case "c": return "BC";
            default: return "?";
        }
    }
}
