// FRAMED CALLS, ABSOLUTE, NOTHING ELSE.
//
// ⚠The frame INTERCEPT (a difference of two differences) proved unmeasurable across
// processes: arms of 86k-165k against 76k-215k. This reports one absolute number over a long
// fixed run instead — ~40M calls whose callees all build a real frame (each has a branch, so
// none can be served frame-free). Absolute totals over a long run are the most drift-robust
// signal this box offers; the correctness marker keeps a faster-but-wrong build out.
public class FrameBench {
    static int b(int x) { if (x < 0) return 1; return x & 7; }
    static long hot(int n) { long a = 0; for (int i = 0; i < n; i++) a += b(i); return a; }
    public static void main(String[] s) {
        hot(2_000_000);
        int n = 40_000_000;
        long t0 = System.nanoTime();
        long r = hot(n);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + r);
        System.out.println("HOTNS=" + ns + " NSPI=" + (ns / n));
    }
}
