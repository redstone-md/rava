// Is `list.add` O(1) amortised here, or O(n)? The native list stores only `$elems` and every
// mutation goes through `iterate_object` -> Vec -> `store_elems`, which writes a fresh array.
// If that is what happens, the cost per append grows with the list, and building a registry of
// a few thousand entries is quadratic. Measured, not inferred.
public class AddCost {
    static Object sink;
    static void run(int n) {
        java.util.ArrayList<Object> l = new java.util.ArrayList<>();
        long t0 = System.nanoTime();
        for (int i = 0; i < n; i++) l.add(Integer.valueOf(i));
        long dt = System.nanoTime() - t0;
        sink = l;
        System.out.println(String.format("n=%6d  %8d ns total  %6d ns/add", n, dt, dt / n));
    }
    public static void main(String[] a) {
        run(1000); run(1000);              // warm
        for (int n : new int[]{16000, 32000, 64000, 128000, 256000}) run(n);
    }
}
