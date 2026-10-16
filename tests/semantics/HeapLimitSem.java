// sem-run-flags: -Xmx64m
import java.util.ArrayList;
import java.util.List;

/** -Xmx bounds the heap: holding more than it allows throws OutOfMemoryError, for large arrays
 *  and for small objects alike, and the program allocates again once it drops what it held.
 *  Each fill runs on a thread of its own, which ends before the next one starts. */
public class HeapLimitSem {
    static boolean fill(int size, int count) throws InterruptedException {
        boolean[] hit = new boolean[1];
        Thread t = new Thread(() -> {
            List<Object> held = new ArrayList<>();
            try {
                for (int i = 0; i < count; i++) {
                    held.add(size < 0 ? new long[2] : new byte[size]);
                }
            } catch (OutOfMemoryError e) {
                held = null;
                hit[0] = true;
            }
        });
        t.start();
        t.join();
        return hit[0];
    }

    public static void main(String[] args) throws InterruptedException {
        boolean large = fill(1 << 20, 1000);
        System.out.println("large arrays past the limit: " + large);
        boolean small = fill(-1, 50_000_000);
        System.out.println("small objects past the limit: " + small);
        long sum = 0;
        for (int round = 0; round < 20; round++) {
            byte[][] a = new byte[16][];
            for (int i = 0; i < a.length; i++) {
                a[i] = new byte[1 << 20];
                a[i][i] = (byte) round;
                sum += a[i][i];
            }
        }
        System.out.println("allocates again: " + sum);
    }
}
