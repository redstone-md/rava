/** Recursive calls with an early result and a slow result sharing one return block.
 *  Reports ns per call after warm-up, best of the measured rounds. */
public class FastReturnBench {
    static int merged(int depth, int bias) {
        int result;
        if (depth <= 0) result = (bias + 1) + 2;
        else result = merged(depth - 1, bias + 1) + merged(depth - 1, bias - 1);
        return result;
    }

    static long constant(int depth) {
        long result;
        if (depth <= 0) result = 17L;
        else result = constant(depth - 1) + constant(depth - 1);
        return result;
    }

    static int multiply(int depth, int bias) {
        int result;
        if (depth <= 0) result = bias * 31;
        else result = multiply(depth - 1, bias + 1) + multiply(depth - 1, bias - 1);
        return result;
    }

    static long multiplyLong(int depth, long bias) {
        long result;
        if (depth <= 0) result = bias * 31L;
        else result = multiplyLong(depth - 1, bias + 1) + multiplyLong(depth - 1, bias - 1);
        return result;
    }

    static int linear(int depth) {
        if (depth <= 0) return 1;
        return linear(depth - 1) + 1;
    }

    public static void main(String[] args) {
        int rounds = args.length > 0 ? Integer.parseInt(args[0]) : 7;
        int depth = 20;
        long calls = (1L << (depth + 1)) - 1;
        double bestMerged = Double.POSITIVE_INFINITY;
        double bestConstant = Double.POSITIVE_INFINITY;
        double bestMultiply = Double.POSITIVE_INFINITY;
        double bestMultiplyLong = Double.POSITIVE_INFINITY;
        double bestLinear = Double.POSITIVE_INFINITY;
        long linearCalls = 0;
        for (int i = 0; i < 4096; i++) linearCalls += 225 + (i & 63);
        long sink = 0;
        for (int r = 0; r < rounds + 3; r++) {
            long start = System.nanoTime();
            sink += merged(depth, r);
            long middle = System.nanoTime();
            sink += constant(depth);
            long end = System.nanoTime();
            sink += multiply(depth, r);
            long multiplyEnd = System.nanoTime();
            sink += multiplyLong(depth, r);
            long multiplyLongEnd = System.nanoTime();
            for (int i = 0; i < 4096; i++) sink += linear(224 + (i & 63));
            long linearEnd = System.nanoTime();
            if (r >= 3) {
                bestMerged = Math.min(bestMerged, (middle - start) / (double) calls);
                bestConstant = Math.min(bestConstant, (end - middle) / (double) calls);
                bestMultiply = Math.min(bestMultiply, (multiplyEnd - end) / (double) calls);
                bestMultiplyLong = Math.min(bestMultiplyLong, (multiplyLongEnd - multiplyEnd) / (double) calls);
                bestLinear = Math.min(bestLinear, (linearEnd - multiplyLongEnd) / (double) linearCalls);
            }
        }
        System.out.printf("merged %.2f ns/call, constant %.2f ns/call, multiply-int %.2f ns/call, multiply-long %.2f ns/call, linear %.3f ns/call, checksum %d%n", bestMerged, bestConstant, bestMultiply, bestMultiplyLong, bestLinear, sink);
    }
}
