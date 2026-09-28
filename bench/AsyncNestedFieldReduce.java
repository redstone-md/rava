import java.util.concurrent.CompletableFuture;
import java.util.function.Supplier;

/** Acceptance for primitive snapshots reached through reference-field chains. */
public final class AsyncNestedFieldReduce {
    private static final int WARM_TRIP = 20_000;

    private AsyncNestedFieldReduce() {}

    private static final class Config {
        final int seed;
        final int trip;

        Config(int seed, int trip) {
            this.seed = seed;
            this.trip = trip;
        }
    }

    private static final class Holder {
        final Config config;

        Holder(Config config) {
            this.config = config;
        }
    }

    static long reduce(int seed, int n) {
        long sum = 0L;
        for (int i = 0; i < n; i++) {
            int term = i * i * 7 + i * 13 + (i & 255) * i
                    - (i >> 2) * i + i * i * i;
            sum += (long) term + seed;
        }
        return sum;
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    public static void main(String[] args) {
        int trip = args.length == 0 ? 500_000_000 : Integer.parseInt(args[0]);
        require(trip > WARM_TRIP, "trip must exceed warm-up size");
        for (int i = 0; i < 8; i++) reduce(i, WARM_TRIP + i);

        Holder leftHolder = new Holder(new Config(1, trip));
        Holder rightHolder = new Holder(new Config(1, trip));
        Supplier<Long> leftTask = () -> Long.valueOf(
                reduce(leftHolder.config.seed, leftHolder.config.trip));
        Supplier<Long> rightTask = () -> Long.valueOf(
                reduce(rightHolder.config.seed, rightHolder.config.trip));

        long started = System.nanoTime();
        CompletableFuture<Long> left = CompletableFuture.supplyAsync(leftTask);
        CompletableFuture<Long> right = CompletableFuture.supplyAsync(rightTask);
        long leftResult = left.join().longValue();
        long rightResult = right.join().longValue();
        long elapsedNanos = System.nanoTime() - started;

        require(leftResult == rightResult, "Future results disagree");
        require(leftResult != 0L, "unexpected zero result");
        System.out.println("SUM_A=" + leftResult + " SUM_B=" + rightResult
                + " CHECKSUM=" + (leftResult ^ rightResult)
                + " FUTURE_NS=" + elapsedNanos);
        System.out.println("ASSERTIONS=ok");
    }
}
