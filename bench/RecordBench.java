// The record twin of CtorBench. `new R(i)` emits invokespecial R.<init>(I)V, whose implicit super
// is `java/lang/Record.<init>()V` — the same guaranteed-empty JDK constructor as Object's, and the
// one MC's registries, codecs and worldgen configs run on every construction.
public class RecordBench {
    record R(int x) {}

    static long loop(int n) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s += new R(i).x();
        long t1 = System.nanoTime();
        if (s == -1) System.out.println("never");
        return t1 - t0;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 2000000;
        for (int r = 1; r <= 4; r++) {
            System.out.println("RUN" + r + "_NS=" + (loop(n) / (double) n));
        }
    }
}
