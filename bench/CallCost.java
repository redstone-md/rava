// ONE call per iteration, at four shapes, DRIFT-CANCELLED, in PICOSECONDS.
//
// Reported in ps/call, not ns: at 8M iterations an integer ns/call figure quantises a
// ~105 ns measurement to 1%, which is the size of the effects being looked for.
//
// ⚠The first version of this bench reported absolutes and was unreadable: the control row
// alone moved 27% across three runs on this box, which is larger than any effect being
// looked for. Each row is now measured back-to-back with the control inside the same
// process and reported as the DIFFERENCE, and the whole schedule repeats so the minimum
// can be taken per row. A drift that moves both halves of a pair cancels out of the
// difference; one that does not shows up as spread across the repeats, which is printed.
public class CallCost {
    static int nop() { return 0; }
    static int arg3(int a, int b, int c) { return a; }
    static int work(int a) { return a + 1; }
    int inst() { return 0; }
    static long c0(int n) { long a = 0; for (int i = 0; i < n; i++) a += i & 1; return a; }
    static long c1(int n) { long a = 0; for (int i = 0; i < n; i++) a += nop(); return a; }
    static long c2(int n) { long a = 0; for (int i = 0; i < n; i++) a += arg3(i, i, i); return a; }
    static long c3(int n) { long a = 0; for (int i = 0; i < n; i++) a += work(i); return a; }
    static long c4(int n, CallCost o) { long a = 0; for (int i = 0; i < n; i++) a += o.inst(); return a; }
    static long run(int w, int n, CallCost o) {
        long t = System.nanoTime();
        switch (w) { case 0: c0(n); break; case 1: c1(n); break; case 2: c2(n); break; case 3: c3(n); break; default: c4(n, o); }
        return System.nanoTime() - t;
    }
    public static void main(String[] s) {
        int n = 8_000_000, reps = 7; CallCost o = new CallCost();
        String[] tag = {"control", "static-nop  ", "static-3arg ", "static-work ", "virtual     "};
        for (int w = 0; w < 3; w++) for (int k = 0; k < 5; k++) run(k, n / 20, o);
        long[][] d = new long[5][reps];
        for (int r = 0; r < reps; r++)
            for (int k = 1; k < 5; k++) { long base = run(0, n, o); long v = run(k, n, o); d[k][r] = (v - base) * 1000L / n; }
        long total = 0;
        for (int k = 1; k < 5; k++) {
            long min = Long.MAX_VALUE; StringBuilder sb = new StringBuilder();
            for (int r = 0; r < reps; r++) { min = Math.min(min, d[k][r]); sb.append(d[k][r]).append(r < reps - 1 ? "/" : ""); }
            total += min;
            System.out.println(tag[k] + " CALLPS=" + min + "   spread " + sb);
        }
        // One aggregate for a paired harness to lock onto: the sum of the four
        // drift-cancelled minima, in picoseconds per call.
        System.out.println("HOTNS=" + total);
    }
}
