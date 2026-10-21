// long-to-int conversions whose results are read as ints (arithmetic, comparisons, int-to-double,
// narrow stores) and as 64-bit values (int-to-long, array indices, phis, calls): the optimizing
// tier leaves the upper half of the first kind alone, which must not leak into the second.
public class Low32Sem {
    static int field;
    static short sfield;
    static byte bfield;

    static int use(int x) { return x * 3; }

    static long mix(long[] seeds, int[] table) {
        long acc = 0;
        for (long s : seeds) {
            int a = (int) (s >> 7);
            int b = (int) s;
            double d = (int) (s >>> 33);
            acc += a + b * 5 - (a ^ b);
            acc += a < b ? 1 : 2;
            acc += (long) (int) (s * 31);
            acc += (long) d;
            field = (int) (s >> 3);
            sfield = (short) (int) (s >> 11);
            bfield = (byte) (int) (s >> 13);
            acc += field + sfield + bfield;
            acc += table[((int) s) & 7];
            acc += use((int) (s >> 17));
            int c = (int) (s >> 40);
            for (int i = 0; i < 3; i++) c += (int) (s >>> i);
            acc += c;
        }
        return acc;
    }

    public static void main(String[] args) {
        long[] seeds = new long[64];
        long x = 0x9E3779B97F4A7C15L;
        for (int i = 0; i < seeds.length; i++) {
            x ^= x << 13; x ^= x >>> 7; x ^= x << 17;
            seeds[i] = x;
        }
        int[] table = {3, 1, 4, 1, 5, 9, 2, 6};
        long sum = 0;
        for (int r = 0; r < 30_000; r++) sum += mix(seeds, table) + r;
        System.out.println(sum);
    }
}
