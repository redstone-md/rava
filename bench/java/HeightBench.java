import java.util.Arrays;
import java.util.Locale;

/** Isolates the terrain-height function used by the Minestom chunk benchmark. */
public final class HeightBench {
    private HeightBench() {}

    private static double noise(long seed, int x, int z) {
        long h = seed ^ (x * 3129871L) ^ (z * 0x5DEECE66DL);
        h = h * h * 42317861L + h * 11L;
        return ((h >> 13) & 0xFFFF) / 65535.0;
    }

    private static double height(int x, int z) {
        double v = 0, f = 1.0 / 64, a = 24;
        for (int o = 0; o < 4; o++) {
            int xi = (int) Math.floor(x * f), zi = (int) Math.floor(z * f);
            double fx = x * f - xi, fz = z * f - zi;
            double n00 = noise(o, xi, zi), n10 = noise(o, xi + 1, zi),
                    n01 = noise(o, xi, zi + 1), n11 = noise(o, xi + 1, zi + 1);
            double nx0 = n00 + (n10 - n00) * fx, nx1 = n01 + (n11 - n01) * fx;
            v += (nx0 + (nx1 - nx0) * fz) * a;
            f *= 2;
            a /= 2;
        }
        return 50 + v;
    }

    private static long sweep(int radius) {
        long hash = 0;
        int limit = radius * 16;
        for (int x = -limit; x < limit; x++) {
            for (int z = -limit; z < limit; z++) {
                hash = hash * 31 + (int) height(x, z);
            }
        }
        return hash;
    }

    public static void main(String[] args) {
        int radius = args.length > 0 ? Integer.parseInt(args[0]) : 24;
        int warmups = args.length > 1 ? Integer.parseInt(args[1]) : 4;
        int reps = args.length > 2 ? Integer.parseInt(args[2]) : 7;
        if (radius < 1 || radius > 64 || warmups < 0 || reps < 1) {
            throw new IllegalArgumentException("expected radius 1..64, warmups >= 0, reps >= 1");
        }

        long expected = 0;
        boolean haveExpected = false;
        for (int i = 0; i < warmups; i++) {
            long actual = sweep(radius);
            if (!haveExpected) {
                expected = actual;
                haveExpected = true;
            } else if (actual != expected) {
                throw new AssertionError("unstable warmup checksum");
            }
        }

        long[] samples = new long[reps];
        long firstSample = 0;
        for (int i = 0; i < reps; i++) {
            long start = System.nanoTime();
            long actual = sweep(radius);
            samples[i] = System.nanoTime() - start;
            if (!haveExpected) {
                expected = actual;
                haveExpected = true;
            } else if (actual != expected) {
                throw new AssertionError("unstable measured checksum");
            }
            if (i == 0) firstSample = samples[i];
        }
        Arrays.sort(samples);
        long median = samples[reps / 2];
        System.out.printf(Locale.ROOT, "HEIGHT radius=%d columns=%d first_ms=%.3f median_ms=%.3f checksum=%016x%n",
                radius, 4L * radius * radius * 256, firstSample / 1_000_000.0, median / 1_000_000.0, expected);
    }
}
