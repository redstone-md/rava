// rava-env: RAVA_OPT_EXCLUDE=RegIntSem.sink,RegIntSem.idx,RegIntSem.mixed
// Int-like arguments in registers reach baseline code sign-extended: optimized 32-bit arithmetic
// leaves the upper half zero, baseline reads the full slot (i2l, long arithmetic, indexing).
public class RegIntSem {
    static long sink(int x) { return (long) x; }
    static long sinkB(byte b, short s, char c, boolean z, int i) { return (long) b * 1000003L + (long) s * 31L + (long) c + (z ? 7L : 0L) + (long) i; }
    static long mixed(long a, int x, double d, int y) { return a + (long) x * 3L + (long) d + (long) y; }
    static int idx(int[] arr, int i) { return arr[i + 5]; }
    static long caller(int x) { return sink(x - 1); }
    static long callerB(int x) { return sinkB((byte) (x - 3), (short) (x - 40000), (char) (x - 1), (x & 1) == 0, x - 100); }
    static long callerM(int x) { return mixed(-5L, x - 10, -2.5, -x); }
    public static void main(String[] a) {
        int[] arr = new int[16];
        for (int i = 0; i < 16; i++) arr[i] = i * 10;
        long s = 0;
        for (int r = 0; r < 200000; r++) {
            int x = r % 7;
            s += caller(x) + callerB(x) + callerM(x) + idx(arr, x - 5);
        }
        System.out.println("sum " + s + " " + caller(0) + " " + callerB(0) + " " + callerM(0));
    }
}
