/** Loop-invariant code motion: a volatile flag read in a spin loop must stay in the loop, a
 *  field the loop stores to (directly, through an alias, or in a call) must be reloaded, a
 *  null object read only on a path the loop never takes must not fault, and invariant values
 *  climb out of nested loops. */
public class OptLicmSem {
    volatile boolean go;
    int count;
    int[] data = {3, 1, 4, 1, 5, 9, 2, 6};
    OptLicmSem other;

    static int spin(OptLicmSem s) {
        int n = 0;
        while (!s.go) n++;
        return n > 0 ? 1 : 1;
    }

    static long aliasStores(OptLicmSem a, OptLicmSem b, int k) {
        long sum = 0;
        for (int i = 0; i < k; i++) {
            sum += a.count;
            b.count += 1;          // b may be a
            sum += a.data.length;
        }
        return sum;
    }

    void bump() { count += 2; }

    static long callStores(OptLicmSem a, int k) {
        long sum = 0;
        for (int i = 0; i < k; i++) {
            sum += a.count;
            a.bump();
        }
        return sum;
    }

    static long maybeNull(OptLicmSem a, int k) {
        long sum = 0;
        for (int i = 0; i < k; i++) {
            if (i > 1000) sum += a.other.count;   // never taken when k <= 1000
            sum += i;
        }
        return sum;
    }

    static long nested(OptLicmSem a, int k) {
        long sum = 0;
        for (int i = 0; i < k; i++) {
            for (int j = 0; j < 8; j++) sum += a.data[j] * (long) a.data.length + (k * 31L);
        }
        return sum;
    }

    /** Invariant in the inner loop only: computed from the outer loop's variable. */
    static long outerDependent(int[] arr, int k) {
        long sum = 0;
        for (int y = 0; y < k; y++) {
            for (int x = 0; x < 4; x++) {
                int base = ((y * 3 + 1) * 2 + 5) & 7;
                sum += arr[(base + x) & 7] * (long) (base + 1);
            }
        }
        return sum;
    }

    public static void main(String[] args) throws Exception {
        OptLicmSem a = new OptLicmSem(), b = new OptLicmSem();
        long s = 0;
        for (int r = 0; r < 20000; r++) {
            s += aliasStores(a, (r & 1) == 0 ? a : b, 50) + callStores(a, 20) + maybeNull(a, 100) + nested(a, 5);
            s += outerDependent(a.data, 9);
        }
        System.out.println(s + " " + a.count + " " + b.count);
        OptLicmSem warm = new OptLicmSem();
        warm.go = true;
        for (int r = 0; r < 50000; r++) s += spin(warm);
        OptLicmSem f = new OptLicmSem();
        Thread t = new Thread(() -> System.out.println("spin " + spin(f)));
        t.start();
        Thread.sleep(200);
        f.go = true;
        t.join();
    }
}
