// Many short-lived threads allocate objects of many sizes and exit while collections run:
// what a finished thread's allocation cache held unused must not be taken for objects.
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicLong;

public class ThreadChurnGcSem {
    static final class Node { final int v; final Object[] kids; long a, b; Node(int v, int n) { this.v = v; kids = new Object[n]; } }
    static volatile Object sink;

    public static void main(String[] args) throws Exception {
        AtomicLong total = new AtomicLong();
        List<Object> keep = new ArrayList<>();
        for (int round = 0; round < 60; round++) {
            Thread[] ts = new Thread[6];
            for (int i = 0; i < ts.length; i++) {
                final int seed = round * 31 + i;
                ts[i] = new Thread(() -> {
                    long s = 0;
                    for (int k = 0; k < 4000; k++) {
                        int n = (seed + k) % 13;
                        Node x = new Node(k, n);
                        for (int j = 0; j < n; j++) x.kids[j] = (j & 1) == 0 ? new byte[(k + j) % 70] : "s" + j;
                        sink = x;
                        s += x.v + x.kids.length;
                    }
                    total.addAndGet(s);
                });
                ts[i].start();
            }
            for (Thread t : ts) t.join();
            for (int k = 0; k < 20000; k++) { Object o = new long[k % 40]; if (k % 997 == 0) keep.add(o); }
        }
        System.out.println("total " + total.get() + " keep " + keep.size());
    }
}
