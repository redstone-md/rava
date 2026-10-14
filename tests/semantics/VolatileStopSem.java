import java.util.concurrent.locks.LockSupport;

// A server loop that runs while a volatile flag is set and parks between ticks, stopped from
// another thread: every read of the flag, interpreted, compiled or after on-stack replacement,
// must see the store. Minecraft's integrated server stops this way when the player quits.
public class VolatileStopSem {
    static class Server {
        volatile boolean running = true;
        volatile boolean stopped;
        long ticks;
        long work;

        void runServer() {
            while (running) {
                for (int i = 0; i < 2000; i++) work += i ^ ticks;
                ticks++;
                if (ticks > 20000) LockSupport.parkNanos(200_000);
            }
            stopped = true;
        }

        void halt() { running = false; }
    }

    static class Plain {
        volatile boolean running = true;

        void spin() {
            long n = 0;
            while (running) n++;
            if (n < 0) System.out.println(n);
        }
    }

    public static void main(String[] args) throws Exception {
        for (int round = 0; round < 3; round++) {
            Server s = new Server();
            Thread t = new Thread(s::runServer, "Server thread");
            t.start();
            Thread.sleep(400 + 300 * round);
            s.halt();
            t.join(10_000);
            System.out.println("round " + round + " stopped " + s.stopped + " alive " + t.isAlive());
            if (t.isAlive()) System.exit(1);

            Plain p = new Plain();
            Thread u = new Thread(p::spin);
            u.start();
            Thread.sleep(300);
            p.running = false;
            u.join(10_000);
            System.out.println("round " + round + " spin stopped " + !u.isAlive());
            if (u.isAlive()) System.exit(1);
        }
    }
}
