// Isolate the most expensive bridge measured (StringBuilder.length() at 718 ns/call) so the
// PC profile decomposes ONE native call instead of five mixed together.
public class SbOnly {
    static long loop(int n, StringBuilder b) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s += b.length();
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }
    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 3000000;
        StringBuilder b = new StringBuilder("hello");
        double v = 0;
        for (int r = 1; r <= 6; r++) v = loop(n, b) / (double) n;
        System.out.println("SB_LEN_NS=" + v);
    }
}
