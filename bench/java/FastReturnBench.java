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

    public static void main(String[] args) {
        int rounds = args.length > 0 ? Integer.parseInt(args[0]) : 7;
        int depth = 20;
        long calls = (1L << (depth + 1)) - 1;
        double bestMerged = Double.POSITIVE_INFINITY;
        double bestConstant = Double.POSITIVE_INFINITY;
        long sink = 0;
        for (int r = 0; r < rounds + 3; r++) {
            long start = System.nanoTime();
            sink += merged(depth, r);
            long middle = System.nanoTime();
            sink += constant(depth);
            long end = System.nanoTime();
            if (r >= 3) {
                bestMerged = Math.min(bestMerged, (middle - start) / (double) calls);
                bestConstant = Math.min(bestConstant, (end - middle) / (double) calls);
            }
        }
        System.out.printf("merged %.2f ns/call, constant %.2f ns/call, checksum %d%n", bestMerged, bestConstant, sink);
    }
}
