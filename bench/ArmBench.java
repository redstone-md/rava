// IS THE LAST-ARM SHORTCUT WORTH WIDENING? A boot measures it at hit=794k / miss=18.7M = 4.1%,
// and the reason is structural: `try_last_arm` can only cache four owner families (Integer/Long,
// String, the Writer family, StringBuilder/StringBuffer). Every collection call — which is what
// the boot's native traffic actually is — misses by construction.
//
// This measures the two sides against each other on the same engine: one call per iteration into
// a CACHED family (StringBuilder.length) and one into an UNCACHED one (ArrayList.get). If the
// shortcut is what makes a native call cheap, the cached side is markedly cheaper; if the plan
// bits already prune the cascade for both, the two are close and widening the arm cache is not
// where the time is.
import java.util.ArrayList;

public class ArmBench {
    static long cached(int n, StringBuilder sb) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s += sb.length();
        long t1 = System.nanoTime();
        if (s == -1) System.out.println("never");
        return t1 - t0;
    }

    static long uncached(int n, ArrayList<Integer> l) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s += l.size();
        long t1 = System.nanoTime();
        if (s == -1) System.out.println("never");
        return t1 - t0;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 2000000;
        StringBuilder sb = new StringBuilder("abc");
        ArrayList<Integer> l = new ArrayList<>();
        l.add(7);
        System.out.println("CACHED_NS=" + (cached(n, sb) / (double) n));
        System.out.println("UNCACHED_NS=" + (uncached(n, l) / (double) n));
    }
}
