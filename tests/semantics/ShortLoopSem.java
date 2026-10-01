/** Short counted loops run without safepoint polls on their back edges: every shape here (up,
 *  down, step 3, inclusive bound, long induction, a test of the next value, a bound next to
 *  MAX_VALUE that must keep its poll) must compute the same sums, and a thread spinning through
 *  them must still reach safepoints while another allocates (collections finish). */
public class ShortLoopSem {
    static volatile boolean stop;

    static long shapes(int x) {
        long s = 0;
        for (int i = 0; i < 7; i++) s += i * x;
        for (int i = 7; i > 0; i--) s ^= i + x;
        for (int i = 0; i <= 30; i += 3) s += i;
        for (int i = 100; i >= 0; i -= 7) s -= i;
        for (long j = 0; j < 64; j++) s += j * j;
        int k = 0;
        do { s += k; } while (++k < 16);
        for (int i = Integer.MAX_VALUE - 9; i < Integer.MAX_VALUE; i++) s += i & 3;
        for (int i = 0; i < 4000; i++) s += (i ^ x) & 1;
        return s;
    }

    public static void main(String[] args) throws Exception {
        long sum = 0;
        for (int x = 0; x < 20000; x++) sum += shapes(x);
        System.out.println(sum);
        long[] spun = new long[1];
        Thread spinner = new Thread(() -> {
            long s = 0;
            while (!stop) s += shapes((int) s);
            spun[0] = s;
        });
        spinner.start();
        long allocated = 0;
        for (int i = 0; i < 3000; i++) allocated += new byte[64 << 10].length >> 10;
        stop = true;
        spinner.join();
        System.out.println("allocated " + allocated + " KiB, spinner done");
    }
}
