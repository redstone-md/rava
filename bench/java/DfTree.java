// A tree of small function objects evaluated per sample: megamorphic interface calls over doubles.
public class DfTree {
    interface F { double compute(int x, int y, int z); }
    record Const(double v) implements F { public double compute(int x, int y, int z) { return v; } }
    record Add(F a, F b) implements F { public double compute(int x, int y, int z) { return a.compute(x, y, z) + b.compute(x, y, z); } }
    record Mul(F a, F b) implements F { public double compute(int x, int y, int z) { return a.compute(x, y, z) * b.compute(x, y, z); } }
    record Clamp(F a, double lo, double hi) implements F { public double compute(int x, int y, int z) { return Math.max(lo, Math.min(hi, a.compute(x, y, z))); } }
    record Noise(long seed, double scale) implements F {
        public double compute(int x, int y, int z) {
            long h = seed ^ (x * 3129871L) ^ (y * 116129781L) ^ (z * 0x5DEECE66DL);
            h = h * h * 42317861L + h * 11L;
            return ((h >> 13) & 0xFFFF) / 65535.0 * scale;
        }
    }
    record YGrad(double from, double to) implements F { public double compute(int x, int y, int z) { return from + (to - from) * (y / 384.0); } }
    static F build(int depth, long seed) {
        if (depth == 0) return (seed & 1) == 0 ? new Noise(seed, 1 + (seed & 7)) : new YGrad(-1, 1);
        F a = build(depth - 1, seed * 31 + 1), b = build(depth - 1, seed * 17 + 3);
        return switch ((int) (seed % 4 + 4) % 4) {
            case 0 -> new Add(a, b);
            case 1 -> new Mul(a, new Const(0.5));
            case 2 -> new Clamp(new Add(a, b), -2, 2);
            default -> new Add(new Mul(a, b), new Const(0.1));
        };
    }
    public static void main(String[] args) {
        long t0 = System.nanoTime();
        F root = build(7, 12345);
        double sum = 0;
        for (int cx = 0; cx < 300; cx++)
            for (int x = 0; x < 16; x += 4) for (int z = 0; z < 16; z += 4) for (int y = -64; y < 320; y += 8)
                sum += root.compute(cx * 16 + x, y, z);
        System.out.println("DF sum=" + sum + " ms=" + (System.nanoTime() - t0) / 1_000_000);
    }
}
