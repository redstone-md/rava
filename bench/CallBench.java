// PRICE ONE GUEST CALL. Ledger row 169 measured 420,608,684 invoke callbacks per boot and
// could not convert that into seconds, because a count is not a cost and `cb/s` bills the
// callee's execution inside the callback it is timing.
//
// This isolates the call itself: each callee does one add and returns, so the loop body is
// dominated by the CALL, not by what is called. Three shapes, because rava serves them by
// three different mechanisms and the ledger should not average over them:
//   static  — monomorphic by construction, the cheapest thing a call can be
//   virtual — one implementation, so a JVM inline cache goes monomorphic
//   iface   — two implementations alternating, which defeats a monomorphic cache
//
// Run the SAME class under both engines and divide. Whatever ratio comes out is the honest
// multiplier on row 169's population.
public class CallBench {
    static int addS(int a, int b) { return a + b; }

    static class Impl { int add(int a, int b) { return a + b; } }
    interface I { int add(int a, int b); }
    static class A implements I { public int add(int a, int b) { return a + b; } }
    static class B implements I { public int add(int a, int b) { return a + b + 0; } }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 20000000;
        Impl v = new Impl();
        I[] pair = { new A(), new B() };

        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s = addS(s, 1);
        long t1 = System.nanoTime();

        int s2 = 0;
        for (int i = 0; i < n; i++) s2 = v.add(s2, 1);
        long t2 = System.nanoTime();

        int s3 = 0;
        for (int i = 0; i < n; i++) s3 = pair[i & 1].add(s3, 1);
        long t3 = System.nanoTime();

        // Print sums so nothing is dead code, then ns/call for each shape.
        System.out.println("SUMS=" + s + "," + s2 + "," + s3 + " N=" + n);
        System.out.println("STATIC_NS=" + ((t1 - t0) / (double) n));
        System.out.println("VIRTUAL_NS=" + ((t2 - t1) / (double) n));
        System.out.println("IFACE_NS=" + ((t3 - t2) / (double) n));
    }
}
