// WHAT DOES A REAL FRAME COST? CallCost's callees are all served frame-free (FLAT-STAT: 100%,
// 0 refusals), so it prices DISPATCH and says nothing about `Frame::new`, the two pooled
// `Vec`s, or the argument copy into locals. These callees cannot be flat-served: each has a
// branch or a second call in the body, so each builds a real interpreter frame.
public class FramedCall {
    static int f0() { return 0; }
    // branch in the body -> not a shape, not straight-line
    static int framed0() { int a = 0; for (int i = 0; i < 1; i++) a += i; return a; }
    static int framed1(int x) { int a = 0; for (int i = 0; i < 1; i++) a += i; return a + x - x; }
    static int framed3(int x, int y, int z) { int a = 0; for (int i = 0; i < 1; i++) a += i; return a + x - x + y - y + z - z; }
    static long c0(int n) { long a = 0; for (int i = 0; i < n; i++) a += i & 1; return a; }
    static long cf0(int n) { long a = 0; for (int i = 0; i < n; i++) a += framed0(); return a; }
    static long cf1(int n) { long a = 0; for (int i = 0; i < n; i++) a += framed1(i); return a; }
    static long cf3(int n) { long a = 0; for (int i = 0; i < n; i++) a += framed3(i, i, i); return a; }
    static long cflat(int n) { long a = 0; for (int i = 0; i < n; i++) a += f0(); return a; }
    static long run(int w, int n) {
        long t = System.nanoTime();
        switch (w) { case 0: c0(n); break; case 1: cflat(n); break; case 2: cf0(n); break; case 3: cf1(n); break; default: cf3(n); }
        return System.nanoTime() - t;
    }
    public static void main(String[] s) {
        int n = 6_000_000, reps = 5;
        String[] tag = {"control", "flat-served ", "framed 0arg ", "framed 1arg ", "framed 3arg "};
        for (int w = 0; w < 3; w++) for (int k = 0; k < 5; k++) run(k, n / 20);
        long total = 0;
        for (int k = 1; k < 5; k++) {
            long min = Long.MAX_VALUE; StringBuilder sb = new StringBuilder();
            for (int r = 0; r < reps; r++) { long b = run(0, n); long v = run(k, n); long d = (v - b) * 1000L / n; min = Math.min(min, d); sb.append(d).append(r < reps - 1 ? "/" : ""); }
            total += min;
            System.out.println(tag[k] + " CALLPS=" + min + "   spread " + sb);
        }
        System.out.println("HOTNS=" + total);
    }
}
