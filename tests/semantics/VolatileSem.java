/** Volatile fields are sequentially consistent (JLS 17.4): in the Dekker pattern
 *  `x = 1; r1 = y` against `y = 1; r2 = x` with volatile x and y, both reads returning 0 is
 *  forbidden. On x86 a store buffer produces exactly that outcome unless a volatile store is
 *  followed by a StoreLoad barrier, so the rounds below (instance and static fields, run
 *  interpreted and compiled) count it. A volatile read also keeps later reads after it. */
public class VolatileSem {
    volatile int x, y;
    static volatile int sx, sy;
    volatile int round;
    int r1, r2;

    static final int ROUNDS = 200_000;

    static int dekker(boolean statics) throws InterruptedException {
        VolatileSem s = new VolatileSem();
        int[] bad = new int[1];
        Thread other = new Thread(() -> {
            for (int i = 1; i <= ROUNDS; i++) {
                while (s.round != i) Thread.onSpinWait();
                if (statics) { sy = 1; s.r2 = sx; } else { s.y = 1; s.r2 = s.x; }
                s.round = -i;
            }
        });
        other.start();
        for (int i = 1; i <= ROUNDS; i++) {
            if (statics) { sx = 0; sy = 0; } else { s.x = 0; s.y = 0; }
            s.round = i;
            if (statics) { sx = 1; s.r1 = sy; } else { s.x = 1; s.r1 = s.y; }
            while (s.round != -i) Thread.onSpinWait();
            if (s.r1 == 0 && s.r2 == 0) bad[0]++;
        }
        other.join();
        return bad[0];
    }

    /** A flag set after data: once the (volatile) flag reads true, the data must be seen. */
    int data;
    volatile boolean ready;

    static int publish() throws InterruptedException {
        int stale = 0;
        for (int round = 0; round < 2000; round++) {
            VolatileSem s = new VolatileSem();
            int before = s.data;
            Thread w = new Thread(() -> { s.data = 42; s.ready = true; });
            w.start();
            while (!s.ready) Thread.onSpinWait();
            if (s.data != 42 || before != 0) stale++;
            w.join();
        }
        return stale;
    }

    public static void main(String[] args) throws InterruptedException {
        System.out.println("instance " + dekker(false));
        System.out.println("static " + dekker(true));
        System.out.println("publish " + publish());
    }
}
