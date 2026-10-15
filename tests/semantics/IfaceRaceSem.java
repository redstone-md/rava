/** A megamorphic interface call site run by several threads at once with receivers of changing
 *  classes: every call must reach its receiver's own implementation (a per-site cache that
 *  paired one thread's class with another thread's target called the wrong method). */
public class IfaceRaceSem {
    interface Shape { int area(); }
    static final class A implements Shape { public int area() { return 1; } }
    static final class B implements Shape { public int area() { return 20; } }
    static final class C implements Shape { public int area() { return 300; } }
    static final class D implements Shape { public int area() { return 4000; } }
    static final class E implements Shape { public int area() { return 50000; } }

    static int call(Shape s) { return s.area(); }

    public static void main(String[] args) throws Exception {
        Shape[] all = { new A(), new B(), new C(), new D(), new E() };
        int[] expect = { 1, 20, 300, 4000, 50000 };
        int threads = 4;
        long[] bad = new long[threads];
        long[] sum = new long[threads];
        Thread[] ts = new Thread[threads];
        for (int t = 0; t < threads; t++) {
            final int id = t;
            ts[t] = new Thread(() -> {
                int seed = id * 7 + 1;
                for (int i = 0; i < 3_000_000; i++) {
                    seed = seed * 1103515245 + 12345;
                    int k = (seed >>> 16) % 5;
                    int r = call(all[k]);
                    if (r != expect[k]) bad[id]++;
                    sum[id] += r;
                }
            });
            ts[t].start();
        }
        long total = 0, wrong = 0;
        for (int t = 0; t < threads; t++) {
            ts[t].join();
            total += sum[t];
            wrong += bad[t];
        }
        System.out.println(total + " wrong " + wrong);
    }
}
