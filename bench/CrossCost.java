// What does ONE crossing of the compiled->host boundary cost, against a guest call in the SAME
// loop shape? Every seam estimate in the ledger rests on this number and it had only ever been
// inferred. ⚠The first version of this bench routed each case through a fresh
// `IntUnaryOperator` lambda, so every timed loop ran at a COLD call site and the empty loop
// measured 1053 ns/op — slower than the native calls it was the control for. Plain static
// methods, one loop each, warmed before timing.
public class CrossCost {
    static int acc;
    static java.util.HashMap<Integer, Integer> map = new java.util.HashMap<>();
    static java.util.ArrayList<Integer> list = new java.util.ArrayList<>();
    static Integer[] boxes = new Integer[64];
    static CrossCost self = new CrossCost();

    static int guestStatic(int x) { return x + 1; }
    int guestVirtual(int x) { return x + 1; }

    static int loopEmpty(int n)   { int s=0; for (int i=0;i<n;i++) s += (i & 63); return s; }
    static int loopStatic(int n)  { int s=0; for (int i=0;i<n;i++) s += guestStatic(i & 63); return s; }
    static int loopVirtual(int n) { int s=0; for (int i=0;i<n;i++) s += self.guestVirtual(i & 63); return s; }
    static int loopListGet(int n) { int s=0; for (int i=0;i<n;i++) s += list.get(i & 63).intValue(); return s; }
    static int loopMapGet(int n)  { int s=0; for (int i=0;i<n;i++) s += map.get(boxes[i & 63]).intValue(); return s; }
    static int loopSize(int n)    { int s=0; for (int i=0;i<n;i++) s += list.size(); return s; }

    static void run(String tag, int mode, int n) {
        long t0 = System.nanoTime();
        int s;
        switch (mode) {
            case 0: s = loopEmpty(n); break;
            case 1: s = loopStatic(n); break;
            case 2: s = loopVirtual(n); break;
            case 3: s = loopListGet(n); break;
            case 4: s = loopMapGet(n); break;
            default: s = loopSize(n); break;
        }
        long dt = System.nanoTime() - t0;
        acc += s;
        System.out.println(String.format("%-22s %7.1f ns/op", tag, (double) dt / n));
    }

    // BALLAST: live objects that stay reachable, so the working set the measured loops run
    // against is large instead of hot-in-L1. The thesis this tests: the boot pays ~727 ns per
    // counted event while a warm microbenchmark of the most expensive event measures 142 ns, and
    // if that 5x gap is cache behaviour rather than a missing population, the SAME loop should
    // slow down by roughly that factor once the live set no longer fits.
    static Object[] ballast;

    static void fill(int objs) {
        if (objs == 0) { ballast = null; return; }
        ballast = new Object[objs];
        for (int i = 0; i < objs; i++) ballast[i] = new int[8];
    }

    public static void main(String[] a) {
        for (int i = 0; i < 64; i++) { boxes[i] = Integer.valueOf(i); map.put(boxes[i], boxes[i]); list.add(boxes[i]); }
        int warm = 300000, n = 5000000;
        for (int m = 0; m <= 5; m++) { loopEmpty(warm); loopStatic(warm); loopVirtual(warm); loopListGet(warm); loopMapGet(warm); loopSize(warm); }
        // ⚠Order and live set are confounded unless the blocks ALTERNATE: the first block of any
        // run is still settling, and reading its numbers as a live-set effect is exactly the
        // mistake this bench exists to avoid making.
        for (int objs : new int[]{0, 2000000, 0, 2000000}) {
            fill(objs);
            System.out.println(objs == 0 ? "--- live set: empty ---" : "--- live set: 2M objects ---");
            run("empty loop", 0, n);
            run("guest static call", 1, n);
            run("guest virtual call", 2, n);
            run("native List.get", 3, n);
            run("native Map.get", 4, n);
        }
        System.out.println("sink " + acc + " " + (ballast == null ? 0 : ballast.length));
    }
}
