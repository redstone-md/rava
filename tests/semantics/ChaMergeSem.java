// Methods nothing overrides, inlined without a receiver test: one check covers every call on a
// receiver that existed when it ran, and a class overriding them loaded later (before the
// compiled code, or while it runs) still dispatches to its own methods.
public class ChaMergeSem {
    static class Base {
        int a() { return 1; }
        int b() { return 2; }
        int c() { return 3; }
    }
    static class Late extends Base {
        @Override int b() { return 20; }
    }
    static class During extends Base {
        @Override int c() { return 300; }
    }

    static int three(Base p) {
        return p.a() * 100 + p.b() * 10 + p.c();
    }

    static Base make(int k) throws Exception {
        return k == 7 ? (Base) Class.forName("ChaMergeSem$During").getDeclaredConstructor().newInstance() : new Base();
    }

    // The second receiver comes from a call that may load an overriding class.
    static int mixed(Base p, int k) throws Exception {
        int x = p.a();
        Base q = make(k);
        return x * 1000 + q.c() * 10 + p.b() + q.a();
    }

    public static void main(String[] args) throws Exception {
        Base base = new Base();
        long s = 0;
        for (int i = 0; i < 200000; i++) s += three(base);
        System.out.println("three base " + s);
        for (int i = 0; i < 200000; i++) s += mixed(base, i & 3);
        System.out.println("mixed base " + s);
        Base late = (Base) Class.forName("ChaMergeSem$Late").getDeclaredConstructor().newInstance();
        System.out.println("three late " + three(late));
        s = 0;
        for (int i = 0; i < 200000; i++) s += three(i % 3 == 0 ? late : base);
        System.out.println("three mixed " + s);
        System.out.println("mixed during " + mixed(base, 7));
        s = 0;
        for (int i = 0; i < 200000; i++) s += mixed(i % 5 == 0 ? late : base, i % 11 == 0 ? 7 : 0);
        System.out.println("mixed all " + s);
    }
}
