// ISOLATE THE FRAME AS AN INTERCEPT, not by subtracting an assumed ns/opcode.
//
// Three callees with the SAME non-flat shape and bodies differing by a known number of
// opcodes. The slope of (cost vs body length) is this engine's per-opcode dispatch on THIS
// body; the intercept at zero body is what the frame itself costs. A flat-served callee is
// included as the no-frame reference.
public class FrameCost {
    static int flat() { return 0; }
    // b0: the minimum non-flat body (a branch makes it un-classifiable).
    static int b0(int x) { if (x < 0) return 1; return 0; }
    // b1: b0 plus 10 extra arithmetic opcodes.
    static int b1(int x) { if (x < 0) return 1; int a = x; a += 1; a -= 1; a += 2; a -= 2; a += 3; return a - x; }
    // b2: b0 plus 20.
    static int b2(int x) { if (x < 0) return 1; int a = x; a += 1; a -= 1; a += 2; a -= 2; a += 3; a -= 3; a += 4; a -= 4; a += 5; a -= 5; return a - x; }
    static long c0(int n) { long a = 0; for (int i = 0; i < n; i++) a += i & 1; return a; }
    static long cflat(int n) { long a = 0; for (int i = 0; i < n; i++) a += flat(); return a; }
    static long cb0(int n) { long a = 0; for (int i = 0; i < n; i++) a += b0(i); return a; }
    static long cb1(int n) { long a = 0; for (int i = 0; i < n; i++) a += b1(i); return a; }
    static long cb2(int n) { long a = 0; for (int i = 0; i < n; i++) a += b2(i); return a; }
    static long run(int w, int n) {
        long t = System.nanoTime();
        switch (w) { case 0: c0(n); break; case 1: cflat(n); break; case 2: cb0(n); break; case 3: cb1(n); break; default: cb2(n); }
        return System.nanoTime() - t;
    }
    public static void main(String[] s) {
        int n = 6_000_000, reps = 5;
        String[] tag = {"control", "flat (no frame)", "framed body+0  ", "framed body+10 ", "framed body+20 "};
        for (int w = 0; w < 3; w++) for (int k = 0; k < 5; k++) run(k, n / 20);
        long[] mins = new long[5];
        for (int k = 1; k < 5; k++) {
            long min = Long.MAX_VALUE; StringBuilder sb = new StringBuilder();
            for (int r = 0; r < reps; r++) { long b = run(0, n); long v = run(k, n); long d = (v - b) * 1000L / n; min = Math.min(min, d); sb.append(d).append(r < reps - 1 ? "/" : ""); }
            mins[k] = min;
            System.out.println(tag[k] + " CALLPS=" + min + "   spread " + sb);
        }
        long slope10 = (mins[4] - mins[2]) / 20;          // ps per opcode, from the +0 vs +20 pair
        System.out.println("PER_OPCODE_PS=" + slope10);
        System.out.println("FRAME_INTERCEPT_PS=" + (mins[2] - mins[1]));
        System.out.println("HOTNS=" + (mins[2] - mins[1]));
    }
}
