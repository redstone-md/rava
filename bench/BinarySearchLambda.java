import java.util.function.IntPredicate;

// Generic higher-order binary search. The hot path intentionally creates a captured
// primitive/array predicate so the benchmark measures lambda + interface dispatch,
// static direct calls, float-array loads, and loop compilation without workload names.
public final class BinarySearchLambda {
    private static int binarySearch(int start, int end, IntPredicate predicate) {
        int remaining = end - start;
        while (remaining > 0) {
            int half = remaining / 2;
            int middle = start + half;
            if (predicate.test(middle)) {
                remaining = half;
            } else {
                start = middle + 1;
                remaining -= half + 1;
            }
        }
        return start;
    }

    private static int find(float[] values, float value) {
        return binarySearch(0, values.length, i -> value < values[i]) - 1;
    }

    private static long hot(float[] values, int iterations) {
        long checksum = 0;
        for (int i = 0; i < iterations; i++) {
            int bits = i * 1_103_515_245 + 12_345;
            float value = (bits & 32_767) * 0.25f;
            checksum = checksum * 31 + find(values, value);
        }
        return checksum;
    }

    public static void main(String[] args) {
        float[] values = new float[8_192];
        for (int i = 0; i < values.length; i++) {
            values[i] = i * 0.5f;
        }

        long warm = 0;
        for (int i = 0; i < 12; i++) {
            warm ^= hot(values, 1_000);
        }

        int iterations = args.length == 0 ? 2_000_000 : Integer.parseInt(args[0]);
        long start = System.nanoTime();
        long checksum = hot(values, iterations);
        long elapsed = System.nanoTime() - start;

        long cases = find(values, 0.0f);
        cases = cases * 31 + find(values, 0.25f);
        cases = cases * 31 + find(values, 0.5f);
        cases = cases * 31 + find(values, values[values.length - 1]);
        cases = cases * 31 + find(values, values[values.length - 1] + 0.25f);

        System.out.println("WARM=" + warm);
        System.out.println("CASES=" + cases);
        System.out.println("SUM=" + checksum);
        System.out.println("HOTNS=" + elapsed);
    }
}
