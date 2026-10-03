// Integer/Long bitCount, numberOfLeadingZeros and numberOfTrailingZeros in optimized code
// (popcnt, lzcnt, tzcnt) agree with plain bit loops, zero and the extremes included.
public class BitCountSem {
    static int hot(long s) {
        int i = (int) s;
        return Integer.bitCount(i) + 3 * Long.bitCount(s) + 5 * Integer.numberOfLeadingZeros(i)
            + 7 * Long.numberOfLeadingZeros(s) + 11 * Integer.numberOfTrailingZeros(i) + 13 * Long.numberOfTrailingZeros(s);
    }
    static int pop(long x, int bits) { int n = 0; for (int b = 0; b < bits; b++) n += (int) (x >>> b) & 1; return n; }
    static int lead(long x, int bits) { int n = 0; for (int b = bits - 1; b >= 0 && ((x >>> b) & 1) == 0; b--) n++; return n; }
    static int trail(long x, int bits) { int n = 0; for (int b = 0; b < bits && ((x >>> b) & 1) == 0; b++) n++; return n; }
    static int ref(long s) {
        long i = (int) s & 0xffffffffL;
        return pop(i, 32) + 3 * pop(s, 64) + 5 * lead(i, 32) + 7 * lead(s, 64) + 11 * trail(i, 32) + 13 * trail(s, 64);
    }
    public static void main(String[] a) {
        long s = 7, acc = 0, bad = 0;
        long[] edge = { 0, -1, 1, Long.MIN_VALUE, Long.MAX_VALUE, Integer.MIN_VALUE, Integer.MAX_VALUE, 1L << 32, 0xffffffffL, 1L << 40 };
        for (int r = 0; r < 300000; r++) {
            s = r < edge.length ? edge[r] : s * 6364136223846793005L + 1442695040888963407L;
            long x = (r & 3) == 0 ? s >>> (r % 61) : s;
            int h = hot(x);
            acc += h;
            if (h != ref(x)) bad++;
        }
        System.out.println(acc + " " + bad);
    }
}
