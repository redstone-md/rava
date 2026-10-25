/** Isolates virtual-thread start/join overhead from a real server's task body. */
public final class VirtualThreadBurstBench {
    private static long task(int i, int work) {
        long x = 0x9e3779b97f4a7c15L ^ i;
        for (int k = 0; k < work; k++) {
            x ^= x >>> 12;
            x ^= x << 25;
            x ^= x >>> 27;
            x *= 0x2545F4914F6CDD1DL;
        }
        return x;
    }

    public static void main(String[] args) throws Exception {
        int count = args.length > 0 ? Integer.parseInt(args[0]) : 2304;
        int work = args.length > 1 ? Integer.parseInt(args[1]) : 1000;
        long[] result = new long[count];
        Thread[] threads = new Thread[count];
        long t0 = System.nanoTime();
        for (int i = 0; i < count; i++) {
            final int id = i;
            threads[i] = Thread.startVirtualThread(() -> result[id] = task(id, work));
        }
        long started = System.nanoTime();
        for (Thread thread : threads) thread.join();
        long t1 = System.nanoTime();
        long hash = 0;
        for (long value : result) hash = Long.rotateLeft(hash, 7) ^ value;
        System.out.printf("VTBURST count=%d work=%d launch_ms=%d join_ms=%d total_ms=%d hash=%016x%n",
                count, work, (started - t0) / 1_000_000, (t1 - started) / 1_000_000,
                (t1 - t0) / 1_000_000, hash);
    }
}
