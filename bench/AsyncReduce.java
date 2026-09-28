import java.util.concurrent.CompletableFuture;
import java.util.function.Supplier;

/**
 * End-to-end acceptance for resident Future tasks reaching a structurally
 * detached JIT reduction. Runtime eligibility must never depend on this class,
 * method, output, or checksum; those identities are benchmark oracles only.
 */
public final class AsyncReduce {
    private static final int WARM_TRIP = 20_000;

    private AsyncReduce() {}

    static long reduce(int n) {
        long sum = 0L;
        for (int i = 0; i < n; i++) {
            int term = i * i * 7 + i * 13 + (i & 255) * i
                    - (i >> 2) * i + i * i * i;
            sum += (long) term;
        }
        return sum;
    }

    private static final class ReduceSupplier implements Supplier<Long> {
        private final int trip;

        ReduceSupplier(int trip) {
            this.trip = trip;
        }

        @Override
        public Long get() {
            return Long.valueOf(reduce(trip));
        }
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    public static void main(String[] args) {
        int trip = args.length == 0 ? 500_000_000 : Integer.parseInt(args[0]);
        require(trip > WARM_TRIP, "trip must exceed warm-up size");

        for (int i = 0; i < 8; i++) reduce(WARM_TRIP + i);

        long started = System.nanoTime();
       CompletableFuture<Long> left = CompletableFuture.supplyAsync(
               new ReduceSupplier(trip));
       CompletableFuture<Long> right = CompletableFuture.supplyAsync(
                new ReduceSupplier(trip));
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
