/** Division and remainder by constants (strength-reduced in the optimizing tier: ±1, powers of
 *  two, magic multiplication) against the same operations by a variable divisor, for int and
 *  long, over values around every edge (0, ±1, MIN, MAX, multiples of the divisor). */
public class OptDivSem {
    static int[] ints(int d) {
        return new int[] {0, 1, -1, 2, -2, d, -d, d - 1, 1 - d, d + 1, -d - 1, 3 * d, -3 * d, 3 * d - 1, Integer.MAX_VALUE, Integer.MIN_VALUE, Integer.MAX_VALUE - 1, Integer.MIN_VALUE + 1, 123456789, -987654321};
    }

    static long mixI(int x, int s) {
        return x / 1 * 31 + x % 1 + (x / -1) * 7 + x % -1
            + (x / 2) * 3 + x % 2 + (x / -2) * 5 + x % -2
            + (x / 1024) * 11 + x % 1024 + (x / -1024) * 13 + x % -1024
            + (x / 3) * 17 + x % 3 + (x / 7) * 19 + x % 7 + (x / 10) * 23 + x % 10
            + (x / -10) * 29 + x % -10 + (x / 641) * 37 + x % 641 + (x / 1000000007) * 41 + x % 1000000007
            + (x / Integer.MIN_VALUE) * 43 + x % Integer.MIN_VALUE + (x / Integer.MAX_VALUE) * 47 + x % Integer.MAX_VALUE
            + (x / 1073741824) * 53 + x % 1073741824 + Math.floorMod(x, 1024) * 59 + Math.floorDiv(x, 16) * 61 + s;
    }

    static long varI(int x, int s, int[] d) {
        return x / d[0] * 31 + x % d[0] + (x / d[1]) * 7 + x % d[1]
            + (x / d[2]) * 3 + x % d[2] + (x / d[3]) * 5 + x % d[3]
            + (x / d[4]) * 11 + x % d[4] + (x / d[5]) * 13 + x % d[5]
            + (x / d[6]) * 17 + x % d[6] + (x / d[7]) * 19 + x % d[7] + (x / d[8]) * 23 + x % d[8]
            + (x / d[9]) * 29 + x % d[9] + (x / d[10]) * 37 + x % d[10] + (x / d[11]) * 41 + x % d[11]
            + (x / d[12]) * 43 + x % d[12] + (x / d[13]) * 47 + x % d[13]
            + (x / d[14]) * 53 + x % d[14] + Math.floorMod(x, d[4]) * 59 + Math.floorDiv(x, d[15]) * 61 + s;
    }

    static long mixL(long x) {
        return x / 1 * 31 + x % 1 + (x / -1) * 7 + x % -1
            + (x / 1024) * 11 + x % 1024 + (x / -4096) * 13 + x % -4096
            + (x / 3) * 17 + x % 3 + (x / 7) * 19 + x % 7 + (x / -10) * 29 + x % -10
            + (x / 0x5851f42d4c957f2dL) * 37 + x % 0x5851f42d4c957f2dL + (x / 1000000007L) * 41 + x % 1000000007L
            + (x / Long.MIN_VALUE) * 43 + x % Long.MIN_VALUE + (x / Long.MAX_VALUE) * 47 + x % Long.MAX_VALUE
            + (x / (1L << 40)) * 53 + x % (1L << 40) + Math.floorMod(x, 1024L) * 59 + (x / 6700417L) * 61 + x % 6700417L;
    }

    static long varL(long x, long[] d) {
        return x / d[0] * 31 + x % d[0] + (x / d[1]) * 7 + x % d[1]
            + (x / d[2]) * 11 + x % d[2] + (x / d[3]) * 13 + x % d[3]
            + (x / d[4]) * 17 + x % d[4] + (x / d[5]) * 19 + x % d[5] + (x / d[6]) * 29 + x % d[6]
            + (x / d[7]) * 37 + x % d[7] + (x / d[8]) * 41 + x % d[8]
            + (x / d[9]) * 43 + x % d[9] + (x / d[10]) * 47 + x % d[10]
            + (x / d[11]) * 53 + x % d[11] + Math.floorMod(x, d[2]) * 59 + (x / d[12]) * 61 + x % d[12];
    }

    public static void main(String[] args) {
        int[] di = {1, -1, 2, -2, 1024, -1024, 3, 7, 10, -10, 641, 1000000007, Integer.MIN_VALUE, Integer.MAX_VALUE, 1073741824, 16};
        long[] dl = {1, -1, 1024, -4096, 3, 7, -10, 0x5851f42d4c957f2dL, 1000000007L, Long.MIN_VALUE, Long.MAX_VALUE, 1L << 40, 6700417L};
        long bad = 0, sum = 0;
        for (int round = 0; round < 3000; round++) {
            for (int d : di) {
                for (int x : ints(d)) {
                    int y = x + round * 7919;
                    long a = mixI(y, round), b = varI(y, round, di);
                    if (a != b) bad++;
                    sum += a;
                }
            }
            for (long d : dl) {
                for (long x : new long[] {0, 1, -1, d, -d, d - 1, 1 - d, 3 * d, Long.MAX_VALUE, Long.MIN_VALUE, Long.MIN_VALUE + 1, 0x123456789abcdefL * round, -round}) {
                    long a = mixL(x), b = varL(x, dl);
                    if (a != b) bad++;
                    sum += a;
                }
            }
        }
        System.out.println("mismatches " + bad + " sum " + sum);
    }
}
