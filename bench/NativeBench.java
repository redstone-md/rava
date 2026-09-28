// WHAT DOES A CALL INTO A NATIVELY-SERVED JDK METHOD COST?
//
// Ledger row 171 measured a guest->guest compiled call at 0.6 ns -- parity with the JVM. But
// only 4.7-10.3% of a boot's samples are inside compiled code and 6-8% in the interpreter, so
// ~80% of the boot is the NATIVE layer. rava implements java.util in Rust; Rust cannot be
// slower than C2 at the same work, so the suspicion is the BRIDGE to it, not the work: every
// guest->native call goes rava_invoke -> resolve -> try_invoke -> an owner cascade of string
// matches (profile: call plumbing ~23% + name resolution ~16%).
//
// Each loop is in its own method called four times, so runs 2-4 are compiled (rava has no OSR;
// run 1 is interpreted and is NOT the number of interest). GUEST is the control already known
// to be 0.6 ns; every other row is the same shape with a natively-served callee.
import java.util.*;

public class NativeBench {
    static int addS(int a, int b) { return a + b; }

    static long guest(int n) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s = addS(s, 1);
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }
    static long listGet(int n, ArrayList<Integer> l) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s += l.get(0);
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }
    static long listSize(int n, ArrayList<Integer> l) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s += l.size();
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }
    static long mapGet(int n, HashMap<String,Integer> m) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s += m.get("k");
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }
    static long sbLen(int n, StringBuilder b) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s += b.length();
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 3000000;
        ArrayList<Integer> l = new ArrayList<>(); l.add(7);
        HashMap<String,Integer> m = new HashMap<>(); m.put("k", 7);
        StringBuilder b = new StringBuilder("hello");
        double g = 0, lg = 0, ls = 0, mg = 0, sb = 0;
        for (int r = 1; r <= 4; r++) {
            g  = guest(n)        / (double) n;
            lg = listGet(n, l)   / (double) n;
            ls = listSize(n, l)  / (double) n;
            mg = mapGet(n, m)    / (double) n;
            sb = sbLen(n, b)     / (double) n;
        }
        System.out.println("GUEST_NS=" + g);
        System.out.println("LIST_GET_NS=" + lg);
        System.out.println("LIST_SIZE_NS=" + ls);
        System.out.println("MAP_GET_NS=" + mg);
        System.out.println("SB_LEN_NS=" + sb);
    }
}
