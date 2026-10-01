/** Deoptimization states keep only the locals the interpreter may still read (bytecode
 *  liveness): locals read only by an exception handler, long and double locals, a local only
 *  iinc touches, and locals dead at the deoptimizing instruction but assigned again later must
 *  all come back right when compiled code leaves for the interpreter mid-method. */
public class OptLiveSem {
    static int sink;

    /** `tag` is read only in the handler; the rare branch deoptimizes, then throws. */
    static int handlerOnly(int[] a, int i, boolean rare) {
        int tag = i * 7 + 3;
        long wide = (long) i << 33;
        double d = i * 0.5;
        try {
            int s = a[i & 7];
            if (rare) {
                sink += s;
                return a[99];
            }
            return s + (int) (wide >>> 33) + (int) d;
        } catch (ArrayIndexOutOfBoundsException e) {
            return -tag;
        }
    }

    /** `n` is dead from the deopt point until it is assigned again. */
    static long reassigned(int x, boolean rare) {
        int n = x * 3;
        long acc = n;
        int k = 0;
        for (int i = 0; i < 16; i++) k += i;
        if (rare) {
            sink += k;
            n = x - 1;
            acc += n * 1000L;
        }
        k++;
        return acc + k;
    }

    static double mixed(double base, int reps, boolean rare) {
        double total = base;
        long count = 0;
        for (int i = 0; i < reps; i++) {
            total += i * 0.25;
            count++;
            if (rare && i == reps - 1) {
                sink += (int) count;
                total *= 2;
            }
        }
        return total + count;
    }

    public static void main(String[] args) {
        int[] a = {1, 2, 3, 4, 5, 6, 7, 8};
        long s = 0;
        double t = 0;
        for (int i = 0; i < 200000; i++) {
            s += handlerOnly(a, i, false) + reassigned(i, false);
            t += mixed(i, 5, false);
        }
        System.out.println(s + " " + t);
        System.out.println(handlerOnly(a, 12345, true) + " " + reassigned(77, true) + " " + mixed(3.5, 6, true));
        long s2 = 0;
        for (int i = 0; i < 50000; i++) s2 += handlerOnly(a, i, i % 1000 == 999) + reassigned(i, i % 997 == 0);
        System.out.println(s2 + " " + sink);
    }
}
