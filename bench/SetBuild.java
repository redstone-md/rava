// HashSet build churn — adds N elements one-by-one. Each add() dedups against
// the existing set. rava's $elems-backed set does an O(n) membership scan per
// add → O(n^2) build; the JVM's HashSet is O(1) per add (hash bucket). This
// isolates whether the O(n^2) set-build is still a real cost (the headline the
// membership-map fix would target) after the ekey_eq constant-factor win.
import java.util.*;
public class SetBuild {
    static long hot(int n, int reps) {
        long acc = 0;
        for (int r = 0; r < reps; r++) {
            HashSet<Integer> s = new HashSet<>();
            for (int i = 0; i < n; i++) s.add(i ^ (r * 31));   // distinct-ish per rep
            acc += s.size();
        }
        return acc;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 8; w++) hot(500, 20);     // warmup
        long t0 = System.nanoTime();
        long s = hot(4_000, 30);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
