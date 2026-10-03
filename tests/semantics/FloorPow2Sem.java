// Math.floorMod / floorDiv by a constant power of two in optimized code (a mask and a shift)
// agree with the definition for negative operands and the extremes.
public class FloorPow2Sem {
    static long mix(long s) { return s * 6364136223846793005L + 1442695040888963407L; }
    static long hot(long s) {
        int i = (int) s, j = (int) (s >>> 17);
        return Math.floorMod(i, 1024) + 3L * Math.floorDiv(i, 64) + Math.floorMod(s, 4096L)
            + 5L * Math.floorDiv(s, 1L << 40) + Math.floorMod(s >> 24, 1024) + Math.floorDiv(s, 8)
            + Math.floorMod(j, 1) + Math.floorDiv(j, 1) + Math.floorMod(i, 1 << 30) + Math.floorMod(j, 3);
    }
    static long ref(long s) {
        int i = (int) s, j = (int) (s >>> 17);
        return fm(i, 1024) + 3L * fd(i, 64) + fm(s, 4096L) + 5L * fd(s, 1L << 40) + fm(s >> 24, 1024)
            + fd(s, 8) + fm(j, 1) + fd(j, 1) + fm(i, 1 << 30) + fm(j, 3);
    }
    static long fd(long x, long y) { long q = x / y; return (x % y != 0 && (x < 0) != (y < 0)) ? q - 1 : q; }
    static long fm(long x, long y) { return x - fd(x, y) * y; }
    public static void main(String[] a) {
        long s = 1, acc = 0, bad = 0;
        long[] edge = { 0, -1, 1, Long.MIN_VALUE, Long.MAX_VALUE, Integer.MIN_VALUE, Integer.MAX_VALUE, -1024, -1025, 1023 };
        for (int r = 0; r < 300000; r++) {
            s = r < edge.length ? edge[r] : mix(s);
            long h = hot(s);
            acc += h;
            if (h != ref(s)) bad++;
        }
        System.out.println(acc + " " + bad);
    }
}
