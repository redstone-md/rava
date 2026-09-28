// WHICH CALL SHAPE STILL COSTS A FRAME? `FrameBench` reports parity with HotSpot (43.3 ms vs
// 49.0 for 40M calls) — but its callee is a STATIC the JIT inlines, so it measures inlining, not
// framing. This separates the shapes the JIT can and cannot inline, so "the frame is 19x" is
// attached to a shape rather than to calls in general.
//
// ⚠Each loop in its own method called repeatedly, so it reaches the compiled tier by entry count.
public class CallShapes {
    interface Op { int f(int x); }
    interface ObjOp { Object g(Object x); }
    static class OA implements ObjOp { public Object g(Object x) { return x; } }
    static class Bean { private int v = 7; private Object o = "s"; int getV() { return v; } Object getO() { return o; } }
    static Bean bean = new Bean();
    static class A implements Op { public int f(int x) { if (x < 0) return 1; return x & 7; } }
    static class B implements Op { public int f(int x) { if (x < 0) return 2; return x & 3; } }
    static class C implements Op { public int f(int x) { if (x < 0) return 3; return x & 1; } }

    static int sf(int x) { if (x < 0) return 1; return x & 7; }
    int vf(int x) { if (x < 0) return 1; return x & 7; }

    static CallShapes self = new CallShapes();
    static Op mono = new A();
    static Op[] poly = { new A(), new B(), new C() };
    static java.util.function.IntUnaryOperator lam = x -> (x < 0 ? 1 : x & 7);
    static ObjOp objMono = new OA();
    static java.util.function.Function<Object,Object> objLam = x -> x;
    static Object arg = new Object();

    static long stat(int n)  { long a = 0; for (int i = 0; i < n; i++) a += sf(i); return a; }
    static long virt(int n)  { long a = 0; for (int i = 0; i < n; i++) a += self.vf(i); return a; }
    static long iface(int n) { long a = 0; for (int i = 0; i < n; i++) a += mono.f(i); return a; }
    static long polyc(int n) { long a = 0; for (int i = 0; i < n; i++) a += poly[i % 3].f(i); return a; }
    static long lamc(int n)  { long a = 0; for (int i = 0; i < n; i++) a += lam.applyAsInt(i); return a; }
    static long objIface(int n) { long a = 0; for (int i = 0; i < n; i++) a += objMono.g(arg) == null ? 0 : 1; return a; }
    static long objLambda(int n) { long a = 0; for (int i = 0; i < n; i++) a += objLam.apply(arg) == null ? 0 : 1; return a; }
    static long getterI(int n)  { long a = 0; for (int i = 0; i < n; i++) a += bean.getV(); return a; }
    static long getterO(int n)  { long a = 0; for (int i = 0; i < n; i++) a += bean.getO() == null ? 0 : 1; return a; }
    static long ctl(int n)   { long a = 0; for (int i = 0; i < n; i++) a += (i & 7); return a; }

    static long time(String nm, int w, int n, int reps) {
        long best = Long.MAX_VALUE, acc = 0;
        for (int r = 0; r < reps; r++) {
            long t0 = System.nanoTime();
            long v = switch (w) {
                case 0 -> stat(n); case 1 -> virt(n); case 2 -> iface(n);
                case 3 -> polyc(n); case 4 -> lamc(n); case 6 -> objIface(n); case 7 -> objLambda(n); case 8 -> getterI(n); case 9 -> getterO(n); default -> ctl(n);
            };
            long dt = System.nanoTime() - t0; acc += v; if (dt < best) best = dt;
        }
        System.out.println(nm + " " + (best / n) + " ns/call  (best " + (best / 1000) + " us)");
        return acc;
    }

    public static void main(String[] s) {
        int n = 2_000_000, reps = 8; long acc = 0;
        acc += time("static (inlinable)  ", 0, n, reps);
        acc += time("virtual monomorphic ", 1, n, reps);
        acc += time("interface monomorphic", 2, n, reps);
        acc += time("interface polymorphic", 3, n, reps);
        acc += time("lambda applyAsInt   ", 4, n, reps);
        acc += time("interface Object arg", 6, n, reps);
        acc += time("lambda   Object arg ", 7, n, reps);
        acc += time("bare getter int     ", 8, n, reps);
        acc += time("bare getter Object  ", 9, n, reps);
        acc += time("control (no call)   ", 5, n, reps);
        System.out.println("acc=" + acc);
    }
}
