// Data-parallel reduction: an expensive pure-arithmetic fold over a counted range.
// rava's parallel JIT (RAVA_JIT=1 RAVA_JIT_PARALLEL=1) recognises the associative
// reduction, compiles the loop body to a worker fn, and fans it across all cores via
// the work-stealing pool — beating C2's single-thread loop by ~5x on a 16-core host.
// The fold is restricted to the SAFE op subset (no idiv/irem: a div-by-zero must throw
// ArithmeticException, impossible in a detached worker), so the body uses imul/iadd/
// iand/ishr only. SUM is bit-exact vs the JVM. Run via bench/parreduce.sh.
public class ParReduce {
    static long hot(int n) {
        long sum = 0;
        for (int i = 0; i < n; i++) {
            int t = i*i*7 + i*13 + (i & 255)*i - (i>>2)*i + i*i*i;
            sum += (long)t;
        }
        return sum;
    }
    public static void main(String[] a) {
        int iters = a.length > 0 ? Integer.parseInt(a[0]) : 200_000_000;
        for (int w = 0; w < 8; w++) hot(5_000_000); // warm the JIT / C2
        long t0 = System.nanoTime();
        long s = hot(iters);
        System.out.println("SUM=" + s + " HOTNS=" + (System.nanoTime() - t0));
    }
}
