// Sum loops compiled into one range sum (opt/reduce.rs): int into int (wrapping), int into
// long, long into long; empty ranges, offsets and lengths on both sides of the inline path.
public class SumLoopSem {
    static int intInt(int[] a, int from, int to) { int s = 7; for (int i = from; i < to; i++) s += a[i]; return s; }
    static long intLong(int[] a, int from, int to) { long s = -3; for (int i = from; i < to; i++) s += a[i]; return s; }
    static long longLong(long[] a, int from, int to) { long s = 11; for (int i = from; i < to; i++) s += a[i]; return s; }
    static int flipped(int[] a) { int s = 0; for (int i = 0; a.length > i; i++) s = a[i] + s; return s; }

    public static void main(String[] args) {
        int[] ia = new int[1000];
        long[] la = new long[1000];
        for (int i = 0; i < ia.length; i++) {
            ia[i] = (i % 3 == 0 ? Integer.MAX_VALUE - i : -i * 7919);
            la[i] = (long) i * 0x1234_5678_9ABL - (i % 5 == 0 ? Long.MAX_VALUE / 3 : 0);
        }
        long h = 0;
        int[][] ranges = { {0, 0}, {5, 5}, {9, 3}, {0, 1}, {3, 18}, {1, 16}, {2, 19}, {7, 22}, {0, 1000}, {13, 999}, {500, 531} };
        for (int round = 0; round < 3000; round++) {
            for (int[] r : ranges) {
                h = h * 31 + intInt(ia, r[0], r[1]);
                h = h * 31 + intLong(ia, r[0], r[1]);
                h = h * 31 + longLong(la, r[0], r[1]);
            }
            h = h * 31 + flipped(ia);
            if (round == 0 || round == 2999) {
                System.out.println(round + " " + h);
                for (int[] r : ranges) System.out.println(r[0] + ".." + r[1] + " " + intInt(ia, r[0], r[1]) + " " + intLong(ia, r[0], r[1]) + " " + longLong(la, r[0], r[1]));
            }
        }
        try {
            intInt(ia, 990, 1010);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("AIOOBE " + e.getMessage());
        }
    }
}
