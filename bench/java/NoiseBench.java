// Array-heavy gradient-noise sampling: the permutation-table + gradient-dot pattern that
// dominates procedural terrain generation. Generic (no application names): a classic Perlin
// value/gradient noise over an int[512] permutation table and a gradient table, summed over
// several octaves across a 3D grid. Exercises array loads with bounds checks, int masking,
// double multiply-add chains and Math.floor — the steady-state hot path, run long enough to
// reach the optimizing tier. Reports the min of several timed reps inside one process.
public class NoiseBench {
    final int[] perm = new int[512];
    static final double[][] GRAD = {
        {1,1,0},{-1,1,0},{1,-1,0},{-1,-1,0},{1,0,1},{-1,0,1},{1,0,-1},{-1,0,-1},
        {0,1,1},{0,-1,1},{0,1,-1},{0,-1,-1},{1,1,0},{0,-1,1},{-1,1,0},{0,-1,-1}
    };
    NoiseBench(long seed) {
        int[] p = new int[256];
        for (int i = 0; i < 256; i++) p[i] = i;
        // Deterministic shuffle.
        long s = seed;
        for (int i = 255; i > 0; i--) {
            s = s * 6364136223846793005L + 1442695040888963407L;
            int j = (int) ((s >>> 33) % (i + 1));
            int t = p[i]; p[i] = p[j]; p[j] = t;
        }
        for (int i = 0; i < 512; i++) perm[i] = p[i & 255];
    }
    static double fade(double t) { return t * t * t * (t * (t * 6 - 15) + 10); }
    static double lerp(double t, double a, double b) { return a + t * (b - a); }
    double grad(int hash, double x, double y, double z) {
        double[] g = GRAD[hash & 15];
        return g[0] * x + g[1] * y + g[2] * z;
    }
    double noise(double x, double y, double z) {
        int xi = (int) Math.floor(x) & 255, yi = (int) Math.floor(y) & 255, zi = (int) Math.floor(z) & 255;
        double xf = x - Math.floor(x), yf = y - Math.floor(y), zf = z - Math.floor(z);
        double u = fade(xf), v = fade(yf), w = fade(zf);
        int a = perm[xi] + yi, aa = perm[a] + zi, ab = perm[a + 1] + zi;
        int b = perm[xi + 1] + yi, ba = perm[b] + zi, bb = perm[b + 1] + zi;
        double x1 = lerp(u, grad(perm[aa], xf, yf, zf), grad(perm[ba], xf - 1, yf, zf));
        double x2 = lerp(u, grad(perm[ab], xf, yf - 1, zf), grad(perm[bb], xf - 1, yf - 1, zf));
        double y1 = lerp(v, x1, x2);
        double x3 = lerp(u, grad(perm[aa + 1], xf, yf, zf - 1), grad(perm[ba + 1], xf - 1, yf, zf - 1));
        double x4 = lerp(u, grad(perm[ab + 1], xf, yf - 1, zf - 1), grad(perm[bb + 1], xf - 1, yf - 1, zf - 1));
        double y2 = lerp(v, x3, x4);
        return lerp(w, y1, y2);
    }
    double octaves(double x, double y, double z, int n) {
        double sum = 0, amp = 1, freq = 1;
        for (int o = 0; o < n; o++) { sum += amp * noise(x * freq, y * freq, z * freq); amp *= 0.5; freq *= 2; }
        return sum;
    }
    double sweep() {
        double sum = 0;
        for (int x = 0; x < 48; x++)
            for (int z = 0; z < 48; z++)
                for (int y = 0; y < 64; y++)
                    sum += octaves(x * 0.013, y * 0.013, z * 0.013, 5);
        return sum;
    }
    public static void main(String[] args) {
        NoiseBench nb = new NoiseBench(12345);
        int warm = 20, reps = 10;
        double guard = 0;
        for (int i = 0; i < warm; i++) guard += nb.sweep();
        long best = Long.MAX_VALUE;
        for (int r = 0; r < reps; r++) {
            long t0 = System.nanoTime();
            guard += nb.sweep();
            long ms = (System.nanoTime() - t0) / 1_000_000;
            if (ms < best) best = ms;
        }
        System.out.println("Noise best_ms=" + best + " guard=" + (long) guard);
    }
}
