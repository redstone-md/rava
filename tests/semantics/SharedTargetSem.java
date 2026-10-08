// A megamorphic call whose receivers all inherit one implementation is inlined behind a test
// of the receiver's vtable entry (and superclass, for an interface): classes loaded later that
// override it, or implement the interface without that superclass, still reach their own code.
public class SharedTargetSem {
    interface Flag { int on(int x); }
    static abstract class Base implements Flag {
        int k = 1;
        public int on(int x) { return x + k; }
        int twice(int x) { return 2 * x + k; }
    }
    static final class A extends Base {} static final class B extends Base {}
    static final class C extends Base {} static final class D extends Base {}
    static final class E extends Base {} static final class F extends Base {}

    static int viaInterface(Flag[] fs, int n) {
        int s = 0;
        for (int i = 0; i < n; i++) s += fs[i % fs.length].on(i);
        return s;
    }

    static int viaClass(Base[] bs, int n) {
        int s = 0;
        for (int i = 0; i < n; i++) s += bs[i % bs.length].twice(i);
        return s;
    }

    public static void main(String[] args) {
        Base[] bs = { new A(), new B(), new C(), new D(), new E(), new F() };
        long acc = 0;
        for (int r = 0; r < 3000; r++) acc += viaInterface(bs, 60) + viaClass(bs, 60);
        System.out.println("warm " + acc);
        // Loaded only now: an override of each, and an implementation outside Base.
        class G extends Base {
            public int on(int x) { return -x; }
            int twice(int x) { return -2 * x; }
        }
        class H implements Flag { public int on(int x) { return 1000 + x; } }
        Flag[] mixed = { new A(), new G(), new H(), new B() };
        Base[] bases = { new A(), new G(), new C() };
        long late = 0;
        for (int r = 0; r < 3000; r++) late += viaInterface(mixed, 40) + viaClass(bases, 30);
        System.out.println("late " + late);
    }
}
