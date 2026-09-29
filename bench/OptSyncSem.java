import java.util.ArrayList;
import java.util.List;

/**
 * Synchronized methods once compiled: instance and static monitors under contention, reentrant
 * calls, exceptions leaving the method (the monitor must be released), and wait/notify.
 */
public class OptSyncSem {
    static int staticCount;
    int count;
    int thrown;

    synchronized void add(int n) {
        count += n;
    }

    synchronized int reentrant(int depth) {
        return depth == 0 ? count : reentrant(depth - 1) + 1;
    }

    synchronized void addOrThrow(int n) {
        if (n % 97 == 0) throw new IllegalArgumentException("n=" + n);
        count += n;
    }

    static synchronized void addStatic(int n) {
        staticCount += n;
    }

    static final Object gate = new Object();
    static int turn;

    static synchronized long mix(long a, long b) {
        return a * 31 + b;
    }

    public static void main(String[] args) throws Exception {
        OptSyncSem shared = new OptSyncSem();
        List<Thread> threads = new ArrayList<>();
        for (int t = 0; t < 4; t++) {
            threads.add(new Thread(() -> {
                for (int i = 1; i <= 50000; i++) {
                    shared.add(1);
                    addStatic(2);
                    try {
                        shared.addOrThrow(i);
                    } catch (IllegalArgumentException e) {
                        synchronized (shared) {
                            shared.thrown++;
                        }
                    }
                }
            }));
        }
        for (Thread t : threads) t.start();
        for (Thread t : threads) t.join();
        System.out.println("count " + shared.count + " static " + staticCount + " thrown " + shared.thrown);
        long m = 0;
        for (int i = 0; i < 100000; i++) m = mix(m, i) + shared.reentrant(3);
        System.out.println("mix " + m);
        // Ping-pong through wait/notify: each side waits for its turn inside a synchronized method.
        Thread other = new Thread(() -> {
            for (int i = 0; i < 2000; i++) waitTurn(1);
        });
        other.start();
        for (int i = 0; i < 2000; i++) waitTurn(0);
        other.join();
        System.out.println("turn " + turn);
    }

    static void waitTurn(int me) {
        synchronized (gate) {
            while (turn % 2 != me) {
                try {
                    gate.wait();
                } catch (InterruptedException e) {
                    throw new RuntimeException(e);
                }
            }
            turn++;
            gate.notifyAll();
        }
    }
}
