// Generic allocation + field traversal — many small short-lived objects.
// Stresses the object model (Phase A: 3 Vecs -> flat buffer) and GC (Phase B:
// generational nursery should make this nearly free).
public class Allocs {
    static final class Point { int x, y, z; Point next;
        Point(int x, int y, int z) { this.x = x; this.y = y; this.z = z; } }
    static long hot(int iters) {
        long acc = 0; Point head = null;
        for (int i = 0; i < iters; i++) {
            Point p = new Point(i, i * 2, i * 3);
            p.next = head;
            head = (i & 15) == 0 ? p : head;                 // keep ~1/16 alive
            acc += p.x + p.y + p.z;
            if ((i & 1023) == 0) head = null;                // drop the chain
        }
        for (Point p = head; p != null; p = p.next) acc ^= p.x;
        return acc;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 15; w++) hot(500_000);            // warmup
        long t0 = System.nanoTime();
        long s = hot(8_000_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
