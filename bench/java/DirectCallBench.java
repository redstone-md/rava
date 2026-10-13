/** The cost of a direct call compiled code does not inline: recursive static methods (a call
 *  cannot inline itself all the way down) with one int argument, four int arguments, and two
 *  doubles with a long. ns per call after warm-up, best of five rounds. */
public class DirectCallBench {
    static int fib(int n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

    static int four(int depth, int a, int b, int c) {
        if (depth == 0) return a ^ b ^ c;
        return four(depth - 1, a + 1, b ^ depth, c - a) + four(depth - 1, b, c, a + depth);
    }

    static double mixed(int depth, double x, long k, double y) {
        if (depth == 0) return x * y + k;
        return mixed(depth - 1, x * 0.5, k + depth, y + 1) - mixed(depth - 1, y, k ^ 3, x);
    }

    /** Calls made by fib(n), four and mixed at depth d: 2^(d+1) - 1. */
    static long fibCalls(int n) { long a = 1, b = 1; for (int i = 1; i < n; i++) { long c = a + b + 1; a = b; b = c; } return b; }

    public static void main(String[] args) {
        int rounds = args.length > 0 ? Integer.parseInt(args[0]) : 5;
        int d = 20;
        long tree = (1L << (d + 1)) - 1;
        double bestFib = 1e9, bestFour = 1e9, bestMixed = 1e9;
        long sink = 0;
        for (int r = 0; r < rounds + 3; r++) {
            long t0 = System.nanoTime(); sink += fib(30); long t1 = System.nanoTime();
            sink += four(d, 1, 2, 3); long t2 = System.nanoTime();
            sink += (long) mixed(d, 1.5, 7, 2.5); long t3 = System.nanoTime();
            if (r >= 3) {
                bestFib = Math.min(bestFib, (t1 - t0) / (double) fibCalls(30));
                bestFour = Math.min(bestFour, (t2 - t1) / (double) tree);
                bestMixed = Math.min(bestMixed, (t3 - t2) / (double) tree);
            }
        }
        System.out.printf("fib %.2f ns/call, four ints %.2f ns/call, mixed %.2f ns/call (%d)%n", bestFib, bestFour, bestMixed, sink & 1);
    }
}
