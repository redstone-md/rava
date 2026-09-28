// ONE invokespecial per iteration, and nothing else that allocates.
//
// CallBench2 measures invokestatic and CallBench measures virtual/interface; nothing in the tree
// measured the CONSTRUCTOR path, which is its own opcode (`invokespecial`) with its own resolve
// and its own cache. It is also the path where the force-native name cascade sat in FRONT of the
// site cache, so every `new` in the program paid seven substring scans over the owner name.
//
// Same shape as CallBench2 on purpose: four runs, per-iteration nanoseconds, no warm-up phase
// that an interpreter cannot finish.
public class CtorBench {
    static final class P {
        final int x;
        P(int x) { this.x = x; }
    }

    static long loop(int n) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s += new P(i).x;
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
