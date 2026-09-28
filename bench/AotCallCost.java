// Isolate an AOT invocation bridge from the same arithmetic in the caller.
// This microbenchmark is not a server speedup or whole-program coverage claim.
public class AotCallCost {
    static int mix(int x, int i) {
        return (x ^ i) * 1664525 + 1013904223;
    }

    static int calls(int n, int seed) {
        int x = seed;
        for (int i = 0; i < n; i++) x = mix(x, i);
        return x;
    }

    static int inline(int n, int seed) {
        int x = seed;
        for (int i = 0; i < n; i++)
            x = (x ^ i) * 1664525 + 1013904223;
        return x;
    }

    public static void main(String[] args) {
        int n = Integer.parseInt(args[0]);
        int seed = Integer.parseInt(args[1]);
        boolean reverse = args.length > 2;
        long start = System.nanoTime();
        int first = reverse ? inline(n, seed) : calls(n, seed);
        long middle = System.nanoTime();
        int second = reverse ? calls(n, seed) : inline(n, seed);
        long end = System.nanoTime();
        if (first != second) throw new AssertionError("call/inline mismatch");
        System.out.println("RESULT=" + first);
        System.out.println("CALL_NS=" + (reverse ? end - middle : middle - start));
        System.out.println("INLINE_NS=" + (reverse ? middle - start : end - middle));
    }
}
