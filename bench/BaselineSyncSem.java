/** Synchronized methods in baseline code: the monitor is taken on entry and released on every
 *  exit (return, exception), re-entrantly, for receivers and class mirrors, under contention,
 *  around wait/notify, and once only when the interpreter finishes a synchronized method in
 *  compiled code (OSR). After each part another thread must be able to take the monitor. */
public class BaselineSyncSem {
    int count;
    static int staticCount;

    synchronized void inc() { count++; }
    synchronized int depth(int n) { return n == 0 ? 0 : 1 + depth(n - 1); }
    synchronized int boom(int k) {
        if (k % 3 == 0) throw new IllegalStateException("k" + k);
        return k;
    }
    static synchronized void sinc() { staticCount++; }
    synchronized long longLoop(int n) {
        long s = 0;
        for (int i = 0; i < n; i++) s += i ^ (s >>> 3);
        return s;
    }
    synchronized Object self() { return this; }
    synchronized void waitFor(int want) throws InterruptedException {
        while (count < want) wait();
    }
    synchronized void bump() { count++; notifyAll(); }

    static boolean free(Object o) throws InterruptedException {
        boolean[] got = {false};
        Thread t = new Thread(() -> { synchronized (o) { got[0] = true; } });
        t.start();
        t.join(5000);
        return got[0];
    }

    static boolean staticFree() throws InterruptedException {
        boolean[] got = {false};
        Thread t = new Thread(() -> { sinc(); got[0] = true; });
        t.start();
        t.join(5000);
        return got[0];
    }

    public static void main(String[] args) throws Exception {
        BaselineSyncSem a = new BaselineSyncSem();
        for (int i = 0; i < 20000; i++) a.inc();
        System.out.println("inc " + a.count + " free " + free(a));
        long d = 0;
        for (int i = 0; i < 5000; i++) d += a.depth(i % 20);
        System.out.println("depth " + d + " free " + free(a));
        int ok = 0, thrown = 0;
        for (int k = 0; k < 9000; k++) {
            try { ok += a.boom(k); } catch (IllegalStateException e) { thrown++; }
        }
        System.out.println("boom " + ok + " " + thrown + " free " + free(a));
        for (int i = 0; i < 20000; i++) sinc();
        System.out.println("static " + staticCount + " free " + staticFree());
        BaselineSyncSem c = new BaselineSyncSem();
        Thread[] ts = new Thread[4];
        for (int t = 0; t < ts.length; t++) {
            ts[t] = new Thread(() -> { for (int i = 0; i < 50000; i++) c.inc(); });
            ts[t].start();
        }
        for (Thread t : ts) t.join();
        System.out.println("contended " + c.count + " free " + free(c));
        BaselineSyncSem o = new BaselineSyncSem();
        System.out.println("osr " + o.longLoop(3_000_000) + " free " + free(o));
        System.out.println("osr again " + o.longLoop(3_000_000) + " free " + free(o));
        for (int i = 0; i < 20000; i++) if (a.self() != a) throw new AssertionError();
        BaselineSyncSem w = new BaselineSyncSem();
        Thread waiter = new Thread(() -> { try { w.waitFor(1000); } catch (InterruptedException e) { } });
        waiter.start();
        for (int i = 0; i < 1000; i++) w.bump();
        waiter.join(10000);
        System.out.println("wait " + w.count + " alive " + waiter.isAlive() + " free " + free(w));
    }
}
