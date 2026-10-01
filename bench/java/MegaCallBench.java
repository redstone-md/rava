/** The cost of a call compiled code cannot inline: a megamorphic virtual call (four receiver
 *  classes, as density-function and surface-rule trees make) and a megamorphic interface call,
 *  from a hot loop in a method compiled by the top tier, against a static call that is
 *  inlined. ns per call after warm-up. */
public class MegaCallBench {
    static abstract class Node { abstract double compute(double x); }
    static final class N0 extends Node { double compute(double x) { return x + 1; } }
    static final class N1 extends Node { double compute(double x) { return x * 0.5; } }
    static final class N2 extends Node { double compute(double x) { return x - 2; } }
    static final class N3 extends Node { double compute(double x) { return -x; } }
    interface F { int apply(int x); }
    static final class F0 implements F { public int apply(int x) { return x + 1; } }
    static final class F1 implements F { public int apply(int x) { return x ^ 3; } }
    static final class F2 implements F { public int apply(int x) { return x - 2; } }
    static final class F3 implements F { public int apply(int x) { return x * 3; } }

    static double virtualLoop(Node[] ns, int n) {
        double s = 0;
        for (int i = 0; i < n; i++) s += ns[i & 3].compute(i);
        return s;
    }

    static int interfaceLoop(F[] fs, int n) {
        int s = 0;
        for (int i = 0; i < n; i++) s += fs[i & 3].apply(i);
        return s;
    }

    public static void main(String[] args) {
        Node[] ns = {new N0(), new N1(), new N2(), new N3()};
        F[] fs = {new F0(), new F1(), new F2(), new F3()};
        double check = 0;
        for (int round = 0; round < 12; round++) {
            long t0 = System.nanoTime();
            for (int k = 0; k < 100; k++) check += virtualLoop(ns, 100_000);
            long t1 = System.nanoTime();
            for (int k = 0; k < 100; k++) check += interfaceLoop(fs, 100_000);
            long t2 = System.nanoTime();
            if (round >= 0) System.out.printf("virtual %.2f ns  interface %.2f ns%n", (t1 - t0) / 1e7, (t2 - t1) / 1e7);
        }
        System.out.printf("check %.1f%n", check);
    }
}
