// M3 9.2e.1 — RavaPool scaling microbench. A hot `for (e : coll) e.compute()` VOID forEach whose
// per-element method is element-ownership-pure (reads its OWN fields, writes its OWN field, no static,
// no cross-element, no other local) — exactly the shape try_fork_iterator_loop → try_parallel_apply_leaf
// forks GIL-free across RavaPool workers. Serial (RAVA_FORK_PARALLEL=0) vs 16-worker (=1); the SUM is
// order-independent so HASH is identical either way (proves determinism + measures wall-time scaling).
import java.util.ArrayList;
import java.util.List;

public class ForkScale {
    static final class Cell {
        long v;
        final int work;
        Cell(long v, int work) { this.v = v; this.work = work; }
        // Element-ownership: reads this.v/this.work, writes this.v. Pure compute (a mixer loop) so the
        // region is COMPUTE-bound — the honest test of core-scaling, not dispatch/iteration overhead.
        void compute() {
            long x = v;
            int w = work;          // hoist the field read OUT of the hot loop (COW-armed path can't LICM it)
            for (int i = 0; i < w; i++) {
                x = x * 6364136223846793005L + 1442695040888963407L;
                x ^= x >>> 29;
                x *= 0xff51afd7ed558ccdL;
                x ^= x >>> 33;
            }
            v = x;
        }
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 200000;
        int work = args.length > 1 ? Integer.parseInt(args[1]) : 400;
        List<Cell> list = new ArrayList<>(n);
        for (int i = 0; i < n; i++) list.add(new Cell(i * 2654435761L, work));

        // Warmup: force compute() + its tree to COMPILE (an uncompiled method can't fork — workers
        // never compile). Runs serial here (pre-fork), so the timed region below forks a hot method.
        for (int w = 0; w < 4; w++) for (Cell c : list) c.compute();

        long t0 = System.nanoTime();
        for (Cell c : list) c.compute();     // <-- FORK TARGET (void forEach; parallel iff RAVA_FORK_PARALLEL=1)
        long t1 = System.nanoTime();

        long sum = 0;
        for (Cell c : list) sum += c.v;      // order-independent reduction → HASH identical serial/parallel
        System.out.println("REGION_NS=" + (t1 - t0) + " REGION_MS=" + ((t1 - t0) / 1_000_000)
                + " N=" + n + " WORK=" + work + " HASH=" + Long.toHexString(sum));
    }
}
