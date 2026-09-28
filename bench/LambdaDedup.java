// Capturing-lambda allocation tracer. Every make() call reaches one indy site
// with one exact int capture; reusing an equivalent closure is permitted by
// JLS 15.27.4. The checksum must match with dedup enabled and disabled.
public final class LambdaDedup {
    interface IntFn { int get(); }

    static IntFn make(int value) {
        return () -> value + 1;
    }

    static long hot(int iterations) {
        long sum = 0;
        for (int i = 0; i < iterations; i++) {
            sum += make(i & 255).get();
        }
        return sum;
    }

    public static void main(String[] args) {
        for (int i = 0; i < 8; i++) hot(100_000);
        long started = System.nanoTime();
        long sum = hot(2_000_000);
        long elapsed = System.nanoTime() - started;
        System.out.println("SUM=" + sum);
        System.out.println("HOTNS=" + elapsed);
    }
}
