import java.util.concurrent.CompletableFuture;
import java.util.function.Supplier;

/**
 * Acceptance workload for CompletableFuture scheduling semantics.
 *
 * Run with expectedPeak=0 to observe scheduling, or 2 where true parallel
 * execution is independently established. Peak measures logical overlap between
 * enter()/exit(); a GIL handoff can produce peak=2 without simultaneous CPU use.
*/
public final class AsyncFuture {
    private static final int SLEEP_MILLIS = 250;
    private static final int CPU_ITERATIONS = 500_000;
    private static final int CPU_SEED_A = 17;
    private static final int CPU_SEED_B = 29;
    private static final long EXPECTED_CPU_A = 63_746_433L;
    private static final long EXPECTED_CPU_B = 63_746_445L;

    private AsyncFuture() {}

    private static final class ConcurrencyProbe {
        private int active;
        private int peak;

        void enter() {
            synchronized (this) {
                active++;
                if (active > peak) peak = active;
            }
        }

        void exit() {
            synchronized (this) {
                active--;
            }
        }

        int peak() {
            synchronized (this) {
                return peak;
            }
        }

        boolean isIdle() {
            synchronized (this) {
                return active == 0;
            }
        }
    }

    private abstract static class TrackedSupplier implements Supplier<Long> {
        private final ConcurrencyProbe probe;

        TrackedSupplier(ConcurrencyProbe probe) {
            this.probe = probe;
        }

        @Override
        public final Long get() {
            probe.enter();
            try {
                return Long.valueOf(run());
            } finally {
                probe.exit();
            }
        }

        protected abstract long run();
    }

    private static final class SleepingSupplier extends TrackedSupplier {
        private final long result;

        SleepingSupplier(ConcurrencyProbe probe, long result) {
            super(probe);
            this.result = result;
        }

        @Override
        protected long run() {
            try {
                Thread.sleep(SLEEP_MILLIS);
            } catch (InterruptedException interrupted) {
                throw new AssertionError("sleep interrupted");
            }
            return result;
        }
    }

    private static final class CpuSupplier extends TrackedSupplier {
        private final int seed;

        CpuSupplier(ConcurrencyProbe probe, int seed) {
            super(probe);
            this.seed = seed;
        }

        @Override
        protected long run() {
            int value = seed;
            for (int i = 0; i < CPU_ITERATIONS; i++) {
                value += i & 255;
            }
            return value;
        }
    }

    private static final class PhaseResult {
        final long left;
        final long right;
        final long elapsedMillis;
        final int peak;

        PhaseResult(long left, long right, long elapsedMillis, int peak) {
            this.left = left;
            this.right = right;
            this.elapsedMillis = elapsedMillis;
            this.peak = peak;
        }

        long checksum() {
            return left ^ right;
        }
    }

    private static PhaseResult runPair(TrackedSupplier left, TrackedSupplier right,
                                       ConcurrencyProbe probe) {
        long started = System.nanoTime();
        CompletableFuture<Long> leftFuture = CompletableFuture.supplyAsync(left);
        CompletableFuture<Long> rightFuture = CompletableFuture.supplyAsync(right);
        long leftResult = leftFuture.join().longValue();
        long rightResult = rightFuture.join().longValue();
        long elapsedMillis = (System.nanoTime() - started) / 1_000_000L;
        require(probe.isIdle(), "supplier remained active after join");
        return new PhaseResult(leftResult, rightResult, elapsedMillis, probe.peak());
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    private static void assertPeak(String phase, int actual, int expected) {
        require(actual >= 1 && actual <= 2, phase + " peak outside [1,2]: " + actual);
        if (expected != 0) {
            require(actual == expected,
                    phase + " peak=" + actual + ", expected=" + expected);
        }
    }

    public static void main(String[] args) {
        int expectedPeak = args.length == 0 ? 0 : Integer.parseInt(args[0]);
        require(expectedPeak >= 0 && expectedPeak <= 2,
                "expectedPeak must be 0 (observe), 1, or 2");

        ConcurrencyProbe sleepProbe = new ConcurrencyProbe();
        PhaseResult sleep = runPair(
                new SleepingSupplier(sleepProbe, 11L),
                new SleepingSupplier(sleepProbe, 22L), sleepProbe);
        require(sleep.left + sleep.right == 33L, "sleep result mismatch");
        assertPeak("sleep", sleep.peak, expectedPeak);
        System.out.println("SLEEP checksum=" + sleep.checksum()
                + " elapsed_ms=" + sleep.elapsedMillis + " peak=" + sleep.peak);

        ConcurrencyProbe cpuProbe = new ConcurrencyProbe();
        PhaseResult cpu = runPair(
                new CpuSupplier(cpuProbe, CPU_SEED_A),
                new CpuSupplier(cpuProbe, CPU_SEED_B), cpuProbe);
        require(cpu.left == EXPECTED_CPU_A, "CPU result A mismatch: " + cpu.left);
        require(cpu.right == EXPECTED_CPU_B, "CPU result B mismatch: " + cpu.right);
        assertPeak("CPU", cpu.peak, expectedPeak);
        System.out.println("CPU checksum=" + cpu.checksum()
                + " elapsed_ms=" + cpu.elapsedMillis + " peak=" + cpu.peak
                + " iterations_each=" + CPU_ITERATIONS);
        System.out.println("ASSERTIONS=ok");
    }
}
