// Differential probe: java.util.Random with FIXED seeds. Worldgen depends on the exact
// sequence, so a divergence here is a different world, not a slower one.
import java.util.*;
import java.util.random.*;
public class RandomSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    static String seq(Random r, int n) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < n; i++) b.append(r.nextInt()).append(' ');
        return b.toString().trim();
    }
    public static void main(String[] a) {
        // --- the specified LCG, from a known seed
        p("nextInt", seq(new Random(42), 5));
        p("bounded", bounded(42, 100, 8));
        p("bounded.pow2", bounded(7, 64, 8));
        p("bounded.range", boundedRange(11, 10, 20, 6));
        p("nextLong", new Random(1).nextLong() + " " + new Random(1).nextLong());
        p("nextDouble", new Random(5).nextDouble() + " " + new Random(5).nextFloat());
        p("nextBoolean", boolSeq(3, 10));
        p("nextGaussian", new Random(9).nextGaussian() + " " + new Random(9).nextGaussian());
        p("setSeed", setSeed());
        p("nextBytes", bytes(13, 6));

        // --- two generators with the same seed agree, and diverge after a draw
        Random x = new Random(100), y = new Random(100);
        x.nextInt();
        p("independent", x.nextInt() + " " + y.nextInt());

        // --- ints()/doubles() streams are the same sequence
        p("stream.ints", Arrays.toString(new Random(4).ints(5).toArray()));
        p("stream.bounded", Arrays.toString(new Random(4).ints(5, 0, 10).toArray()));
        p("stream.doubles", new Random(4).doubles(2).sum());

        // --- the seed scrambling is specified, so equal seeds give equal streams
        p("scramble", new Random(0).nextInt() + " " + new Random(-1).nextInt()
            + " " + new Random(Long.MAX_VALUE).nextInt());

        // --- errors
        try { new Random(1).nextInt(0); p("bound.zero", "NO THROW"); }
        catch (Exception e) { p("bound.zero", e.getClass().getName()); }
        try { new Random(1).nextInt(-5); p("bound.neg", "NO THROW"); }
        catch (Exception e) { p("bound.neg", e.getClass().getName()); }
        try { new Random(1).nextInt(9, 3); p("bound.inverted", "NO THROW"); }
        catch (Exception e) { p("bound.inverted", e.getClass().getName()); }

        // --- SplittableRandom, also specified
        p("splittable", new SplittableRandom(21).nextInt() + " " + new SplittableRandom(21).nextLong());

        // --- Math.random exists and stays in range
        double m = Math.random();
        p("mathRandom", (m >= 0 && m < 1));
    }
    static String bounded(long seed, int bound, int n) {
        Random r = new Random(seed);
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < n; i++) b.append(r.nextInt(bound)).append(' ');
        return b.toString().trim();
    }
    static String boundedRange(long seed, int lo, int hi, int n) {
        Random r = new Random(seed);
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < n; i++) b.append(r.nextInt(lo, hi)).append(' ');
        return b.toString().trim();
    }
    static String boolSeq(long seed, int n) {
        Random r = new Random(seed);
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < n; i++) b.append(r.nextBoolean() ? '1' : '0');
        return b.toString();
    }
    static String setSeed() {
        Random r = new Random(1);
        int first = r.nextInt();
        r.setSeed(1);
        return first + " " + r.nextInt();
    }
    static String bytes(long seed, int n) {
        byte[] bs = new byte[n];
        new Random(seed).nextBytes(bs);
        return Arrays.toString(bs);
    }
}
