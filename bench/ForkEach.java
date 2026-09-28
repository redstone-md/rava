// M3 G3.5 target: a per-element method that allocates an escaping PRIMITIVE ARRAY.
//
//   for (Pt p : pts) p.compute();   // compute(): int[] t = new int[4]; ...; this.arr = t;
//
// The int[] is allocated via the fork's parallel-array-region (workers claim pre-reserved heap
// slots atomically — the heap array Vecs aren't concurrent-safe to push) and escapes into
// this.arr. Deterministic SUM over the array elements confirms the parallel fork + array region
// are bit-exact vs the JVM.
public class ForkEach {
    static final class Pt {
        int a, b;
        int[] arr;
        Pt(int a, int b) { this.a = a; this.b = b; this.arr = null; }
        void compute() {
            // Array allocated BEFORE the loop, filled INSIDE it (the real VoxelShape/BitSet
            // shape). The durable single-assign array `t` stays fresh across the loop join.
            int[] t = new int[4];
            int x = this.a;
            int y = this.b;
            for (int i = 0; i < 1000; i++) {
                x = mix(x, y);
                y = y ^ (x >>> 3);
                t[i & 3] = x;
            }
            this.arr = t;
        }
    }

    static int mix(int x, int y) {
        return x * 31 + y;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 200000;
        java.util.ArrayList<Pt> pts = new java.util.ArrayList<>();
        for (int i = 0; i < n; i++) {
            pts.add(new Pt(i, i * 7 + 1));
        }
        long best = Long.MAX_VALUE;
        long sum = 0;
        for (int rep = 0; rep < 12; rep++) {
            long t0 = System.nanoTime();
            for (Pt p : pts) {
                p.compute();
            }
            long dt = System.nanoTime() - t0;
            if (dt < best) best = dt;
            sum = 0;
            for (Pt p : pts) {
                sum += p.arr[0] + p.arr[1] + p.arr[2] + p.arr[3];
            }
        }
        System.out.println("HOTNS=" + best + " SUM=" + sum + " N=" + n);
    }
}
