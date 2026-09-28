// Repro for the Getstatic-Inline crash (UnsatisfiedLinkError: Integer.isEmpty(),
// receiver = Int(0) instead of a ref). Each hot* isolates ONE newly-admitted inline
// shape. Run with RAVA_JIT_TIER0=1 (flips require_all=false => the PARTIAL inline map,
// the regime the crash needs).
//
// NOTE the traps this file is written around:
//  * `static final String X = ""` is a COMPILE-TIME CONSTANT — javac emits `ldc`, not
//    getstatic, so it tests nothing. Every static here is assigned in <clinit> from a
//    non-constant expression, which forces a real getstatic.
//  * the JIT threshold counts METHOD invocations, so each hot* is called in a loop from
//    main rather than once.
public class GsInline {

    // --- shape A: the exact Shapes.empty() -> VoxelShape.isEmpty() shape ---
    // `getstatic <ref>; areturn` inlined, then a VIRTUAL call on the result. This is
    // where the crash landed: the receiver arrived as Int(0) and dispatched to Integer.
    static class Shape {
        final boolean e;
        Shape(boolean e) { this.e = e; }
        boolean isEmpty() { return e; }
    }

    static final Shape EMPTY = new Shape(true);

    static Shape empty() { return EMPTY; }

    static int hotA(int n) {
        int c = 0;
        for (int i = 0; i < n; i++) {
            if (empty().isEmpty()) c++;
        }
        return c;
    }

    // --- shape B: enum values() = getstatic $VALUES + invokevirtual clone() + checkcast ---
    enum Axis { X, Y, Z }

    static Axis[] axes() { return Axis.values(); }

    static int hotB(int n) {
        int c = 0;
        for (int i = 0; i < n; i++) c += axes().length;
        return c;
    }

    // --- shape C: leaf getstatic of a static final ARRAY ref, then read it ---
    static final int[] TABLE = new int[] { 5, 6, 7 };

    static int[] table() { return TABLE; }

    static int hotC(int n) {
        int c = 0;
        for (int i = 0; i < n; i++) c += table()[i % 3];
        return c;
    }

    // --- shape D: getstatic + invokestatic (the ImprovedNoise.gradDot shape = the WIN) ---
    static final int[] GRAD = new int[] { 2, 3, 5 };

    static int dot(int[] g, int x) { return g[0] * x + g[1]; }

    static int gradDot(int i, int x) { return dot(GRAD, x + i); }

    static int hotD(int n) {
        int c = 0;
        for (int i = 0; i < n; i++) c += gradDot(i & 1, i & 7);
        return c;
    }

    // --- shape E: getstatic of a ref reached THROUGH a chain, virtual call on it ---
    // Mirrors initCache's `Shapes.empty()` feeding a shape variable used later.
    static final Shape[] SHAPES = new Shape[] { new Shape(false), new Shape(true) };

    static Shape pick(int i) { return SHAPES[i & 1]; }

    static int hotE(int n) {
        int c = 0;
        for (int i = 0; i < n; i++) if (pick(i).isEmpty()) c++;
        return c;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 20000;
        int reps = 64; // clear the invocation threshold for each hot* method
        long a = 0, b = 0, c = 0, d = 0, e = 0;
        System.out.println("A_start");
        for (int r = 0; r < reps; r++) a += hotA(n);
        System.out.println("A=" + a);
        System.out.println("B_start");
        for (int r = 0; r < reps; r++) b += hotB(n);
        System.out.println("B=" + b);
        System.out.println("C_start");
        for (int r = 0; r < reps; r++) c += hotC(n);
        System.out.println("C=" + c);
        System.out.println("D_start");
        for (int r = 0; r < reps; r++) d += hotD(n);
        System.out.println("D=" + d);
        System.out.println("E_start");
        for (int r = 0; r < reps; r++) e += hotE(n);
        System.out.println("E=" + e);
        System.out.println("GSINLINE_OK");
    }
}
