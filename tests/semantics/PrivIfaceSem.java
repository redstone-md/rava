// Private interface methods, which javac calls with invokeinterface: hot callers inline them as
// statically bound (no receiver profile needed), and a null receiver still throws.
public class PrivIfaceSem {
    interface Ext {
        int base();
        private int twice() { return base() * 2; }
        default int value(int k) { return twice() + k; }
    }
    interface Ext2 extends Ext {
        private int twice() { return base() * 3; }
        default int value2(int k) { return twice() + value(k); }
    }
    static final class A implements Ext2 { public int base() { return 5; } }
    static class B implements Ext { int b; B(int b) { this.b = b; } public int base() { return b; } }
    static class C extends B { C() { super(7); } public int base() { return 11; } }

    static int run(Ext e, int k) { return e.value(k); }
    static int run2(Ext2 e, int k) { return e.value2(k); }

    public static void main(String[] args) {
        Ext[] es = {new A(), new B(3), new C()};
        long sum = 0;
        for (int i = 0; i < 3_000_000; i++) {
            sum += run(es[i % 3], i & 7);
            sum += run2((Ext2) es[0], i & 3);
        }
        System.out.println("sum=" + sum);
        int npe = 0;
        for (int i = 0; i < 200_000; i++) {
            try { run(i % 1000 == 0 ? null : es[i % 3], 1); } catch (NullPointerException e) { npe++; }
        }
        System.out.println("npe=" + npe);
    }
}
