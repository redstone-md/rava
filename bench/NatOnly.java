// Isolate ONE native call made FROM COMPILED CODE.
//
// Ledger row 197: a compiled->compiled guest call is 0.7 ns and a compiled->native call is
// 28.4 ns, and row 196 showed 96% of the calls that leave machine code go to a natively-served
// class. ArrayList.size() is the cheapest possible case -- fu_fastpath already claims to serve
// it off the $elems backing without the rava_invoke synthetic frame -- so whatever this profile
// shows IS the boundary, with no real work mixed in.
//
// Six rounds so runs 2+ are compiled (rava has no OSR; run 1 is interpreted and is not the
// number of interest).
import java.util.ArrayList;

public class NatOnly {
    static long loop(int n, ArrayList<Integer> l) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s += l.size();
        long t1 = System.nanoTime();
        if (s == -1) System.out.print("");
        return t1 - t0;
    }

    public static void main(String[] a) {
        int n = a.length > 0 ? Integer.parseInt(a[0]) : 8000000;
        ArrayList<Integer> l = new ArrayList<>();
        l.add(7);
        double v = 0;
        for (int r = 1; r <= 6; r++) v = loop(n, l) / (double) n;
        System.out.println("NAT_NS=" + v);
    }
}
