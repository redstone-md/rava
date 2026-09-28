// Is a compiled guest ARRAY STORE / FIELD WRITE really ~100 ns here? `bench/AddCost.java` reads
// 273 ns for an inlined `ArrayList.add` whose body is a field read, a compare, an array store
// and a field write — against HotSpot's ~2. That would contradict "our compiled static call is
// 1.1 ns vs HotSpot's 0.8", so measure the primitives directly, with no boxing in the loop.
public class StoreCost {
    static Object[] arr = new Object[1024];
    static int fld;
    static Object payload = new Object();
    static Object sink;

    static void stores(int n) {
        Object p = payload;
        long t0 = System.nanoTime();
        for (int i = 0; i < n; i++) arr[i & 1023] = p;
        long dt = System.nanoTime() - t0;
        System.out.println(String.format("%-22s %6d ns/op", "aastore", dt / n));
    }
    static void fields(int n) {
        long t0 = System.nanoTime();
        for (int i = 0; i < n; i++) fld = i;
        long dt = System.nanoTime() - t0;
        System.out.println(String.format("%-22s %6d ns/op", "putstatic int", dt / n));
    }
    static void reads(int n) {
        long s = 0;
        long t0 = System.nanoTime();
        for (int i = 0; i < n; i++) s += arr[i & 1023] == null ? 0 : 1;
        long dt = System.nanoTime() - t0;
        sink = Long.valueOf(s);
        System.out.println(String.format("%-22s %6d ns/op", "aaload+cmp", dt / n));
    }
    static final class Box { int v; Object r; }
    static Box box = new Box();

    static void instFields(int n) {
        Box b = box;
        long t0 = System.nanoTime();
        for (int i = 0; i < n; i++) b.v = i;
        long dt = System.nanoTime() - t0;
        System.out.println(String.format("%-22s %6d ns/op", "putfield int", dt / n));
    }
    static void instRefFields(int n) {
        Box b = box;
        Object p = payload;
        long t0 = System.nanoTime();
        for (int i = 0; i < n; i++) b.r = p;
        long dt = System.nanoTime() - t0;
        System.out.println(String.format("%-22s %6d ns/op", "putfield ref", dt / n));
    }
    static void instReads(int n) {
        Box b = box;
        long s = 0;
        long t0 = System.nanoTime();
        for (int i = 0; i < n; i++) s += b.v;
        long dt = System.nanoTime() - t0;
        sink = Long.valueOf(s);
        System.out.println(String.format("%-22s %6d ns/op", "getfield int", dt / n));
    }

    static void boxes(int n) {
        long s = 0;
        long t0 = System.nanoTime();
        for (int i = 0; i < n; i++) s += Integer.valueOf(i & 255).intValue();
        long dt = System.nanoTime() - t0;
        sink = Long.valueOf(s);
        System.out.println(String.format("%-22s %6d ns/op", "Integer.valueOf", dt / n));
    }
    public static void main(String[] a) {
        int n = 4000000;
        stores(200000); fields(200000); reads(200000); boxes(200000);
        instFields(200000); instRefFields(200000); instReads(200000);
        stores(n); fields(n); reads(n); boxes(n);
        instFields(n); instRefFields(n); instReads(n);
    }
}
