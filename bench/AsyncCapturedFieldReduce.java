import java.util.concurrent.CompletableFuture;
import java.util.function.Supplier;

/**
 * Acceptance for a LambdaMetafactory closure that captures a reference but
 * exposes only one primitive field to a detached kernel. Runtime admission must
 * depend exclusively on bytecode, descriptors, field resolution and effects.
 */
public final class AsyncCapturedFieldReduce {
    private static final int WARM_TRIP = 20_000;

    private AsyncCapturedFieldReduce() {}

    private static final class Config {
        final int trip;

        Config(int trip) {
            this.trip = trip;
        }
    }

    static long reduce(int n) {
        long sum = 0L;
        for (int i = 0; i < n; i++) {
            int term = i * i * 7 + i * 13 + (i & 255) * i
                    - (i >> 2) * i + i * i * i;
            sum += (long) term;
        }
        return sum;
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    public static void main(String[] args) {
        int trip = args.length == 0 ? 500_000_000 : Integer.parseInt(args[0]);
        require(trip > WARM_TRIP, "trip must exceed warm-up size");
        for (int i = 0; i < 8; i++) reduce(WARM_TRIP + i);

        Config leftConfig = new Config(trip);
        Config rightConfig = new Config(trip);
        Supplier<Long> leftTask = () -> Long.valueOf(reduce(leftConfig.trip));
        Supplier<Long> rightTask = () -> Long.valueOf(reduce(rightConfig.trip));

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
