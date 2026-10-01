// Differential probe: threads, synchronization and thread-local state. Deterministic on
// purpose — every thread is joined, nothing races, no timing is asserted.
//
// TWO LINES ARE EXPECTED TO DIVERGE, both from the engine's threading model rather than
// from a defect — a third means something broke:
//
//   threadLocal  the JVM prints `main init`: the second thread has its own value, so it
//                sees the initial one. This engine runs a started thread INLINE (or as a
//                cooperative green thread), so the "other thread" IS the main thread and
//                reads `main`. Real per-thread storage needs real threads.
//
//   monitor      `LOCK.wait(1)` outside a synchronized block is an
//                IllegalMonitorStateException on the JVM. monitorenter/monitorexit are
//                no-ops here (one guest thread under the GIL), so there is no ownership
//                to check. wait/notify are no-ops that return at once; a
//                `while (!ready) wait();` loop would spin rather than block.
// Machine-readable form of the two notes above, for bench\sem_all.ps1.
// DIVERGES: threadLocal
// DIVERGES: monitor
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
public class ThreadSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    static String ex(Callable r) {
        try { r.call(); return "NO THROW"; }
        catch (Throwable t) { return t.getClass().getName(); }
    }
    interface Callable { void call() throws Exception; }
    static int shared = 0;
    static final Object LOCK = new Object();
    static synchronized void bump() { shared++; }
    public static void main(String[] a) throws Exception {
        // --- the current thread
        Thread cur = Thread.currentThread();
        p("current", (cur.getName() != null) + " " + cur.isAlive() + " " + (cur.getId() > 0));

        // --- a thread runs, and join waits for it
        StringBuilder log = new StringBuilder();
        Thread t = new Thread(() -> log.append("ran"), "probe-1");
        p("beforeStart", t.getName() + " " + t.isAlive() + " " + t.getState());
        t.start();
        t.join();
        p("afterJoin", log + " " + t.isAlive() + " " + t.getState());

        // --- synchronized counting across joined threads is exact
        shared = 0;
        List<Thread> ts = new ArrayList<>();
        for (int i = 0; i < 4; i++) {
            Thread x = new Thread(() -> { for (int k = 0; k < 250; k++) bump(); });
            ts.add(x); x.start();
        }
        for (Thread x : ts) x.join();
        p("synchronized", shared);

        // --- a synchronized BLOCK on a shared monitor
        shared = 0;
        Thread s1 = new Thread(() -> { for (int k = 0; k < 500; k++) synchronized (LOCK) { shared++; } });
        s1.start(); s1.join();
        p("syncBlock", shared);

        // --- atomics across threads
        AtomicInteger ai = new AtomicInteger();
        Thread[] as = new Thread[3];
        for (int i = 0; i < as.length; i++) { as[i] = new Thread(() -> { for (int k = 0; k < 100; k++) ai.incrementAndGet(); }); as[i].start(); }
        for (Thread x : as) x.join();
        p("atomic", ai.get());

        // --- ThreadLocal is per thread
        ThreadLocal<String> tl = ThreadLocal.withInitial(() -> "init");
        tl.set("main");
        String[] seen = new String[1];
        Thread tt = new Thread(() -> seen[0] = tl.get());
        tt.start(); tt.join();
        p("threadLocal", tl.get() + " " + seen[0]);
        tl.remove();
        p("threadLocal.remove", tl.get());

        // --- interrupt status
        Thread.currentThread().interrupt();
        p("interrupt", Thread.interrupted() + " " + Thread.interrupted());

        // --- an exception in a thread does not reach the starter
        Thread bad = new Thread(() -> { throw new IllegalStateException("boom"); });
        bad.setUncaughtExceptionHandler((th, e) -> log.append("|caught:").append(e.getMessage()));
        bad.start(); bad.join();
        p("uncaught", log.toString());

        // --- an executor, driven to completion
        ExecutorService pool = Executors.newFixedThreadPool(2);
        Future<Integer> f = pool.submit(() -> 41 + 1);
        p("executor", f.get() + " " + f.isDone());
        List<java.util.concurrent.Callable<Integer>> cs = List.of(() -> 1, () -> 2);
        int sum = 0;
        for (Future<Integer> r : pool.invokeAll(cs)) sum += r.get();
        p("invokeAll", sum);
        pool.shutdown();
        p("shutdown", pool.isShutdown() + " " + pool.awaitTermination(5, TimeUnit.SECONDS));

        // --- illegal monitor state
        p("monitor", ex(() -> LOCK.wait(1)));
        p("start.twice", ex(() -> t.start()));
    }
}
