// One construct in a loop: allocate a fieldless object. Nothing else to attribute to.
public class AllocOnly {
    static final class Empty { }
    static long hot(int n) { long a = 0; for (int i = 0; i < n; i++) { Empty e = new Empty(); if (e != null) a++; } return a; }
    public static void main(String[] s) {
        hot(1_000_000);
        long t0 = System.nanoTime(); long r = hot(20_000_000); long ns = System.nanoTime() - t0;
        System.out.println("N=" + r + " HOTNS=" + ns + " NSPI=" + (ns / 20_000_000));
    }
}
