// Universal acceptance workload for the bytecode/dataflow-gated counted-loop fork.
// The timed loop has no calls, allocation, shared metadata access, or loop-carried state:
// each iteration writes one primitive array cell selected by the induction variable.
public final class ParallelNumeric {
    private static void fill(long[] out, int n, long seed) {
        for (int i = 0; i < n; i++) {
            long x = seed + i;
            x = (x ^ (x >>> 30)) * 0xbf58476d1ce4e5b9L;
            x = (x ^ (x >>> 27)) * 0x94d049bb133111ebL;
            x ^= x >>> 31;
            x = (x ^ (x << 17)) * 0x9e3779b97f4a7c15L;
            x = (x ^ (x >>> 23)) * 0xd6e8feb86659fd93L;
            out[i] = x ^ (x >>> 29);
        }
    }

    private static long checksum(long[] values) {
        long hash = 1;
        for (int i = 0; i < values.length; i++) {
            hash = hash * 1_000_003L + values[i];
        }
        return hash;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 2_000_000;
        long[] values = new long[n];
        long start = System.nanoTime();
        fill(values, n, 0x1234_5678_9abc_def0L);
        long elapsed = System.nanoTime() - start;
        System.out.println("HASH=" + Long.toHexString(checksum(values))
                + " FILL_NS=" + elapsed + " N=" + n);
    }
}
