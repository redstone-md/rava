// COST OF A NATIVE CALL FROM COMPILED CODE.
//
// Ledger row 196: 96% of the calls that leave compiled code go to a natively-served class with
// no compiled body. The reachable repair is to make that boundary cheap (HotSpot bakes an
// intrinsic id at class load and C1 emits a direct call), so the first number needed is the
// boundary's price when the CALLER is compiled -- every loop here is re-entered so runs 2+ are
// compiled, and run 1 (interpreted, no OSR) is not the number of interest.
import java.util.ArrayList;

public class NatCall {
    static int guestAdd(int a, int b) { return a + b; }

    static long guest(int n) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s = guestAdd(s, 1);
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }
    static long nativeSize(int n, ArrayList<Integer> l) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s += l.size();
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }
    public static void main(String[] a) {
        int n = a.length > 0 ? Integer.parseInt(a[0]) : 4000000;
        ArrayList<Integer> l = new ArrayList<>(); l.add(7);
        double g = 0, nv = 0;
        for (int r = 1; r <= 5; r++) { g = guest(n) / (double) n; nv = nativeSize(n, l) / (double) n; }
        System.out.println("GUEST_NS=" + g);
        System.out.println("NATIVE_NS=" + nv);
    }
}
