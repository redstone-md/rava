// DOES A SECOND ENTRY RUN COMPILED? Row 170 measured ~390 ns per call and attributed it to a
// compiled->compiled call. The PC profile then showed `opcodes::run` at 13.8% and JIT-STATS
// showed `main compiled_runs=0`: the CALLER was interpreted, because RAVA has no OSR and a
// method already on the stack never switches to its compiled body.
//
// This isolates that claim. The same loop method is called twice with identical work. Call 1
// is the once-entered case; by call 2 the method is compiled and re-entered from scratch.
// If no-OSR is the explanation, call 2 is dramatically faster and call 1 is not.
public class CallBench2 {
    static int addS(int a, int b) { return a + b; }

    static long loop(int n) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s = addS(s, 1);
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
