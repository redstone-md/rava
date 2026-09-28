// OpBench plus exactly one array load per iteration.
//
// The array opcodes stayed in the cold half of the dispatch loop (they can throw), and nothing in
// the tree measured them: OpBench is pure arithmetic, CallBench's interface loop contains an
// aaload but confounds it with dispatch. The difference between this and OpBench is one `iaload`
// and the index arithmetic feeding it.
public class ArrayBench {
    static long loop(int n, int[] a) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s += a[i & 15];
        long t1 = System.nanoTime();
        if (s == -1) System.out.println("never");
        return t1 - t0;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 20000000;
        int[] a = new int[16];
        for (int i = 0; i < 16; i++) a[i] = i;
        for (int r = 1; r <= 4; r++) {
            System.out.println("RUN" + r + "_NS=" + (loop(n, a) / (double) n));
        }
    }
}
