// Deep recursion until StackOverflowError, caught and survived repeatedly: compiled prologues
// raise it before linking their frame, and the frames below unwind normally.
public class StackOverflowSem {
    static int depth;
    static int down(int n) { depth = n; return down(n + 1) + 1; }
    static double downD(double x) { return downD(x + 1) * 0.5; }
    synchronized int downSync(int n) { return downSync(n + 1) + 1; }

    public static void main(String[] args) {
        int caught = 0;
        for (int round = 0; round < 50; round++) {
            try { down(0); } catch (StackOverflowError e) { caught++; }
            try { downD(0); } catch (StackOverflowError e) { caught++; }
            StackOverflowSem s = new StackOverflowSem();
            try { s.downSync(0); } catch (StackOverflowError e) { caught++; }
            // The monitor was released on the way out.
            Thread t = new Thread(() -> { synchronized (s) { } });
            t.start();
            try { t.join(); } catch (InterruptedException e) { throw new RuntimeException(e); }
        }
        System.out.println("caught " + caught + " deep " + (depth > 1000));
        System.out.println("after " + down2(10));
    }
    static int down2(int n) { return n == 0 ? 0 : down2(n - 1) + n; }
}
