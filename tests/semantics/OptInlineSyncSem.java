/** Synchronized methods inlined by the optimizing tier: the monitor is taken at the call and
 *  released at each return, and when the inlined body deoptimizes (a receiver class its profile
 *  did not have, an exception, an uncommon branch), released once the interpreter finishes it.
 *  Re-entrant nesting, static methods, wait/notify and contention too. After each part another
 *  thread must be able to take the monitor. */
public class OptInlineSyncSem {
    interface Shape { int area(); }
    static final class Sq implements Shape { public int area() { return 4; } }
    static final class Tri implements Shape { public int area() { return 3; } }
    static final class Odd implements Shape { public int area() { return 7; } }

    int count;
    static int scount;

    synchronized int add(Shape s) { count += s.area(); return count; }
    synchronized int inner(int k) { return k + 1; }
    synchronized int outer(int k) { return inner(k) + inner(k + 1); }
    synchronized int risky(int k) {
        if (k % 1000 == 999) throw new IllegalArgumentException("k" + k);
        return k & 7;
    }
    synchronized int rare(int k) {
        if (k == 77_777) return count * 2;   // never taken while compiling
        return k & 3;
    }
    static synchronized int sadd(int k) { scount += k; return scount; }
    synchronized void bump() { count++; notifyAll(); }
    synchronized void waitFor(int want) throws InterruptedException { while (count < want) wait(); }

    static int callAdd(OptInlineSyncSem o, Shape s) { return o.add(s); }
    static int callOuter(OptInlineSyncSem o, int k) { return o.outer(k); }
    static int callRisky(OptInlineSyncSem o, int k) { return o.risky(k); }
    static int callRare(OptInlineSyncSem o, int k) { return o.rare(k); }
    static int callStatic(int k) { return sadd(k); }

    static boolean free(Object o) throws InterruptedException {
        boolean[] got = {false};
        Thread t = new Thread(() -> { synchronized (o) { got[0] = true; } });
        t.start();
        t.join(5000);
        return got[0];
    }

    public static void main(String[] args) throws Exception {
        OptInlineSyncSem a = new OptInlineSyncSem();
        Shape sq = new Sq(), tri = new Tri();
        long s = 0;
        for (int i = 0; i < 200_000; i++) s += callAdd(a, (i & 1) == 0 ? sq : tri);
        s += callAdd(a, new Odd());   // unseen receiver: deoptimizes inside the inlined body
        for (int i = 0; i < 1000; i++) s += callAdd(a, (i & 1) == 0 ? sq : new Odd());
        System.out.println("add " + s + " " + a.count + " free " + free(a));
        long n = 0;
        for (int i = 0; i < 200_000; i++) n += callOuter(a, i & 15);
        System.out.println("nested " + n + " free " + free(a));
        long ok = 0, thrown = 0;
        for (int k = 0; k < 300_000; k++) {
            try { ok += callRisky(a, k); } catch (IllegalArgumentException e) { thrown++; }
        }
        System.out.println("risky " + ok + " " + thrown + " free " + free(a));
        long r = 0;
        for (int k = 0; k < 200_000; k++) r += callRare(a, k);
        System.out.println("rare " + r + " free " + free(a));
        long st = 0;
        for (int k = 0; k < 200_000; k++) st += callStatic(k & 3);
        boolean[] sgot = {false};
        Thread tt = new Thread(() -> { synchronized (OptInlineSyncSem.class) { sgot[0] = true; } });
        tt.start();
        tt.join(5000);
        System.out.println("static " + st + " " + scount + " free " + sgot[0]);
        OptInlineSyncSem c = new OptInlineSyncSem();
        Thread[] ts = new Thread[4];
        for (int t = 0; t < ts.length; t++) {
            ts[t] = new Thread(() -> { for (int i = 0; i < 100_000; i++) callAdd(c, sq); });
            ts[t].start();
        }
        for (Thread t : ts) t.join();
        System.out.println("contended " + c.count + " free " + free(c));
        OptInlineSyncSem w = new OptInlineSyncSem();
        Thread waiter = new Thread(() -> { try { w.waitFor(2000); } catch (InterruptedException e) { } });
        waiter.start();
        for (int i = 0; i < 2000; i++) w.bump();
        waiter.join(10000);
        System.out.println("wait " + w.count + " alive " + waiter.isAlive() + " free " + free(w));
    }
}
