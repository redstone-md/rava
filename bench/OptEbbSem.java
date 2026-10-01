/** Field values and value numbers carried across inlined bodies and guard paths (extended basic
 *  blocks): a store through an alias inside an inlined callee, a store in one branch of a type
 *  guard, a call that writes the field, and a volatile read in between must all be seen. */
public class OptEbbSem {
    int f;
    volatile int v;
    OptEbbSem other;

    void bump(OptEbbSem o) { o.f += 1; }
    int read() { return f; }
    void viaCall(OptEbbSem o) { if (o.f == Integer.MIN_VALUE) System.out.println("never"); else touch(o); }
    static void touch(OptEbbSem o) { o.f = o.f * 3 + 1; }

    interface Shape { int area(OptEbbSem s); }
    static final class Sq implements Shape { public int area(OptEbbSem s) { s.f += 2; return s.f; } }
    static final class Ci implements Shape { public int area(OptEbbSem s) { return s.f * 2; } }

    static long run(OptEbbSem a, OptEbbSem alias, Shape sh, int n) {
        long acc = 0;
        for (int i = 0; i < n; i++) {
            int x = a.f;              // load
            a.bump(alias);            // inlined: alias may be a
            int y = a.read();         // inlined: must reload when alias == a
            acc += x * 31L + y;
            int z = sh.area(a);       // guarded inline: one shape writes a.f
            acc += z + a.f;
            int w = a.f;
            int vv = a.v;             // volatile read: later loads read memory again
            acc += w + vv + a.f;
            a.viaCall(a);             // writes a.f on the out-of-line path
            acc += a.f;
            a.f &= 0xffff;
        }
        return acc;
    }

    public static void main(String[] args) {
        OptEbbSem a = new OptEbbSem(), b = new OptEbbSem();
        Shape[] shapes = {new Sq(), new Ci()};
        long sum = 0;
        for (int i = 0; i < 4000; i++) {
            a.v = i;
            sum += run(a, (i & 1) == 0 ? a : b, shapes[(i >> 2) & 1], 50);
        }
        System.out.println(sum + " " + a.f + " " + b.f);
    }
}
