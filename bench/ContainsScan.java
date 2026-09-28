// contains/indexOf scan churn — builds an ArrayList then scans it via
// contains()/indexOf() many times. The general native path used to CLONE the
// whole $elems backing (iterate_object) into a throwaway Vec on EVERY call;
// the no-clone fast path scans the heap array directly. This bench isolates
// that read path (no autobox churn in the inner scan beyond the needle).
import java.util.*;
public class ContainsScan {
    static long hot(int n, int scans) {
        ArrayList<Integer> l = new ArrayList<>();
        for (int i = 0; i < n; i++) l.add(i * 2);     // even values 0..2n
        long acc = 0;
        for (int s = 0; s < scans; s++) {
            int needle = (s * 7) % (n * 2);            // hits ~half the time
            if (l.contains(needle)) acc += needle;
            acc += l.indexOf(needle);
        }
        return acc;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 10; w++) hot(2_000, 2_000);   // warmup
        long t0 = System.nanoTime();
        long s = hot(4_000, 20_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
