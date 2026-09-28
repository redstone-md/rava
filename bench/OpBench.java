// CallBench2 MINUS THE CALL. Same loop, same counters, same printout — `s = addS(s, 1)` replaced
// by `s = s + 1`, so the difference between the two benchmarks is one interpreted invokestatic
// and its return, and nothing else.
//
// Run both under rava JIT=0 and under `java -Xint` and the pair separates the two questions the
// interpreter arc keeps conflating: is the gap to HotSpot in the per-OPCODE dispatch, or in the
// CALL protocol? A ratio that is the same on both benchmarks says dispatch; a ratio that jumps on
// CallBench2 says the call.
public class OpBench {
    static long loop(int n) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s = s + 1;
        long t1 = System.nanoTime();
        if (s == -1) System.out.println("never");
        return t1 - t0;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 5000000;
        for (int r = 1; r <= 4; r++) {
            System.out.println("RUN" + r + "_NS=" + (loop(n) / (double) n));
        }
    }
}
