import java.util.concurrent.CyclicBarrier;

/** Allocation against a retained old heap with sparse or dense reference updates. Arguments:
 * old MiB, rounds, allocating threads, update stride (1 = dense), churn MiB per round.
 * Run each configuration in a fresh process; RAVA_GC_LOG reports young-pause phase timings. */
public class YoungCardsBench {
    static final class Value { final long id; Value(long id) { this.id = id; } }
    static final class Sink {
        volatile Object value;
        long p0, p1, p2, p3, p4, p5, p6, p7, p8, p9, p10, p11, p12, p13, p14;
    }
    static Object[][] old;
    static Sink[] sinks;

    static void churn(int writer, int count) {
        Sink sink = sinks[writer];
        for (int i = 0; i < count; i++) sink.value = new byte[1024];
    }

    public static void main(String[] args) throws Exception {
        int oldMiB = args.length > 0 ? Integer.parseInt(args[0]) : 128;
        int rounds = args.length > 1 ? Integer.parseInt(args[1]) : 8;
        int writers = args.length > 2 ? Integer.parseInt(args[2]) : 1;
        int stride = args.length > 3 ? Integer.parseInt(args[3]) : 256;
        int churnMiB = args.length > 4 ? Integer.parseInt(args[4]) : 64;
        if (oldMiB <= 0 || rounds <= 0 || writers <= 0 || stride <= 0 || churnMiB <= 0) {
            throw new IllegalArgumentException("arguments must be positive");
        }
        old = new Object[oldMiB * 256][];
        for (int i = 0; i < old.length; i++) old[i] = new Object[1024];
        sinks = new Sink[writers];
        for (int i = 0; i < writers; i++) sinks[i] = new Sink();
        System.gc();

        int count = (churnMiB * 1024 + writers - 1) / writers;
        CyclicBarrier start = new CyclicBarrier(writers);
        CyclicBarrier done = new CyclicBarrier(writers);
        Thread[] workers = new Thread[writers - 1];
        for (int i = 0; i < workers.length; i++) {
            final int writer = i + 1;
            workers[i] = new Thread(() -> {
                try {
                    for (int r = 0; r < rounds; r++) {
                        start.await();
                        churn(writer, count);
                        done.await();
                    }
                } catch (Exception e) {
                    throw new RuntimeException(e);
                }
            });
            workers[i].start();
        }
        System.err.println("[young-cards] measurement begins");
        long total = 0;
        for (int r = 0; r < rounds; r++) {
            for (int i = 0; i < old.length; i += stride) {
                old[i][(r * 131) & 1023] = new Value(1_000_000L * r + i);
            }
            long begin = System.nanoTime();
            start.await();
            churn(0, count);
            done.await();
            long elapsed = System.nanoTime() - begin;
            total += elapsed;
            System.out.printf("round %d ms %.3f%n", r, elapsed / 1e6);
        }
        for (Thread worker : workers) worker.join();
        long sum = 0;
        for (int i = 0; i < old.length; i += stride) {
            for (int r = 0; r < Math.min(rounds, 1024); r++) {
                Object v = old[i][(r * 131) & 1023];
                if (v != null) sum += ((Value) v).id;
            }
        }
        System.out.printf("oldMiB %d writers %d stride %d total ms %.3f check %d%n", oldMiB, writers, stride, total / 1e6, sum);
    }
}
