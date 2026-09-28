// `new Empty()` is NOT one operation: it is the 0xbb opcode PLUS Empty.<init>
// PLUS Object.<init> — two interpreter frames. Q2 runs the same two frames with
// no allocation, so Q1-Q2 prices the allocation alone.
public class NewVsFrame {
    static final class Empty { }
    static int nop0() { return 0; }
    static int nop1() { return nop0(); }
    static long q1(int n) { long a = 0; for (int i = 0; i < n; i++) { Empty e = new Empty(); if (e != null) a++; } return a; }
    static long q2(int n) { long a = 0; for (int i = 0; i < n; i++) { int e = nop1(); if (e == 0) a++; } return a; }
    static long q3(int n) { long a = 0; for (int i = 0; i < n; i++) { if (i >= 0) a++; } return a; }
    static void t(String tag, int which, int n) {
        long t0 = System.nanoTime(); long s = which == 1 ? q1(n) : which == 2 ? q2(n) : q3(n);
        long ns = System.nanoTime() - t0;
        System.out.println(tag + " SUM=" + s + " NSPI=" + (ns / n));
    }
    public static void main(String[] a) {
        int n = 10_000_000;
        for (int w = 0; w < 3; w++) { q1(n / 20); q2(n / 20); q3(n / 20); }
        t("Q1 new+2frames ", 1, n); t("Q2 2frames     ", 2, n); t("Q3 loop only   ", 3, n);
    }
}
