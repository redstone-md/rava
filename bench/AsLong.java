// ChunkPos.asLong's exact shape under the JIT: (x & 0xFFFFFFFFL) | ((z & 0xFFFFFFFFL) << 32)
// over negative/positive mixes, hot enough to compile.
public class AsLong {
    static long hot(int n) {
        long acc = 0;
        for (int i = 0; i < n; i++) {
            int x = (i & 1) == 0 ? -(i & 63) : (i & 63);
            int z = (i & 2) == 0 ? -(i & 31) : (i & 31);
            long k = (x & 0xFFFFFFFFL) | ((z & 0xFFFFFFFFL) << 32);
            acc = acc * 31 + k;
        }
        return acc;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 15; w++) hot(100_000);
        System.out.println("SUM=" + hot(1_000_000));
        // Spot values:
        int[][] ps = {{0,0},{-1,0},{0,-1},{-1,-1},{5,-3}};
        for (int[] p : ps) {
            long k = (p[0] & 0xFFFFFFFFL) | ((p[1] & 0xFFFFFFFFL) << 32);
            System.out.println("K(" + p[0] + "," + p[1] + ")=" + Long.toHexString(k));
        }
    }
}
