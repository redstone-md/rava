// Loop-invariant tests whose one side only deoptimizes leave the loop as a guard on entry:
// iterating a list (its modification count), and a branch never taken while the loop ran hot.
// When the guard fails the loop must behave as with the test inside, side effects and
// ConcurrentModificationException included.
import java.util.*;

public class GuardHoistSem {
    static long walk(List<Integer> l, int[] out) {
        long s = 0;
        int k = 0;
        for (int x : l) {
            s += x;
            out[k++ & 7] = (int) s;
        }
        return s;
    }
    static long flagged(int[] a, boolean flag, int[] out) {
        long s = 0;
        for (int i = 0; i < a.length; i++) {
            if (flag) out[i & 7] -= 1;
            s += a[i];
        }
        return s;
    }
    static long mutating(List<Integer> l) {
        long s = 0;
        for (int x : l) {
            s += x;
            if (x == 7) l.add(100);
        }
        return s;
    }
    public static void main(String[] args) {
        List<Integer> l = new ArrayList<>();
        for (int i = 0; i < 20; i++) l.add(i);
        int[] a = new int[20], out = new int[8];
        for (int i = 0; i < a.length; i++) a[i] = i * 3;
        long acc = 0;
        for (int r = 0; r < 100000; r++) acc += walk(l, out) + flagged(a, false, out) + (r % 3 == 0 ? mutating(List.of(1, 2, 3)) : 0);
        System.out.println(acc + " " + Arrays.toString(out));
        System.out.println(flagged(a, true, out) + " " + Arrays.toString(out));
        try { System.out.println(mutating(new ArrayList<>(List.of(5, 6, 7, 8)))); } catch (ConcurrentModificationException e) { System.out.println("CME"); }
        Iterator<Integer> it = l.iterator();
        it.next();
        l.remove(0);
        try { it.next(); System.out.println("no CME"); } catch (ConcurrentModificationException e) { System.out.println("CME2"); }
        for (int r = 0; r < 100000; r++) acc += walk(l, out) + flagged(a, (r & 1023) == 0, out);
        System.out.println(acc + " " + Arrays.toString(out));
    }
}
