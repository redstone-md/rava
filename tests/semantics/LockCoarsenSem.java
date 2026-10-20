// Lock coarsening (opt/coarsen.rs) of partially unrolled synchronized loops: the merged regions
// still exclude other threads, an exception inside a region releases the lock, and a waiter
// woken from inside a coarsened loop sees every update.
public class LockCoarsenSem {
    static final Object LOCK = new Object();
    static int counter;
    static long sum;
    int field;

    synchronized void inc() { field++; }

    static void bump(int n) { for (int i = 0; i < n; i++) synchronized (LOCK) { counter++; } }

    static void addAll(int[] a, int n) { for (int i = 0; i < n; i++) synchronized (LOCK) { sum += a[i]; } }

    static int risky(int[] a, int n) {
        int done = 0;
        try {
            for (int i = 0; i < n; i++) synchronized (LOCK) { counter += a[i] / a[i]; done++; }
        } catch (ArithmeticException e) {
            System.out.println("caught at " + done + " holding " + Thread.holdsLock(LOCK));
        }
        return done;
    }

    public static void main(String[] args) throws Exception {
        Thread[] ts = new Thread[4];
        for (int k = 0; k < ts.length; k++) {
            ts[k] = new Thread(() -> bump(1_000_000));
            ts[k].start();
        }
        for (Thread t : ts) t.join();
        System.out.println("counter " + counter);

        int[] a = new int[1000];
        for (int i = 0; i < a.length; i++) a[i] = i * 7 - 300;
        Thread[] us = new Thread[3];
        for (int k = 0; k < us.length; k++) {
            us[k] = new Thread(() -> { for (int r = 0; r < 2000; r++) addAll(a, a.length); });
            us[k].start();
        }
        for (Thread t : us) t.join();
        System.out.println("sum " + sum);

        int[] b = new int[100];
        java.util.Arrays.fill(b, 3);
        for (int r = 0; r < 20000; r++) risky(b, b.length);
        b[57] = 0;
        System.out.println("risky " + risky(b, b.length) + " " + Thread.holdsLock(LOCK));

        LockCoarsenSem o = new LockCoarsenSem();
        Thread w = new Thread(() -> {
            synchronized (o) {
                while (o.field < 5_000_000) {
                    try { o.wait(); } catch (InterruptedException e) { return; }
                }
                System.out.println("woken " + o.field);
            }
        });
        w.start();
        for (int r = 0; r < 50; r++) {
            for (int i = 0; i < 100_000; i++) o.inc();
            synchronized (o) { o.notifyAll(); }
        }
        w.join();
        System.out.println("field " + o.field);
    }
}
