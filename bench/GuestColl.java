// TRACER BULLET: is a collection written in GUEST BYTECODE, compiled by our own JIT, faster
// than the Rust native that replaces java.util today?
//
// The earlier attempt put real JDK 25 `java.util` bytecode on the classpath and read
// `List.size` 65 -> 369 ns, then died on `UnsatisfiedLinkError: jdk/internal/util/
// Preconditions.checkIndex`. That experiment could not separate "guest bytecode is slower"
// from "that particular bytecode never got compiled and drags jdk.internal behind it".
//
// This one has no such confound: GuestList is a plain array-backed list written here, with no
// dependency outside primitives and Object[], so whatever it costs is what OUR engine costs to
// run a collection as bytecode. Same process, same run, against java.util.ArrayList.
public class GuestColl {
    static final class GuestList {
        Object[] a = new Object[16];
        int n = 0;
        int size() { return n; }
        Object get(int i) { return a[i]; }
        void add(Object v) {
            if (n == a.length) {
                Object[] b = new Object[n * 2];
                System.arraycopy(a, 0, b, 0, n);
                a = b;
            }
            a[n++] = v;
        }
    }

    // Open-addressing map, again with no dependency outside primitives and arrays. `Map.get` is
    // the most expensive native on the table (458 ns/call against HotSpot's 3), so if guest
    // bytecode wins here too the result is not an artefact of one collection.
    // ⚠The FIRST version of this arm keyed on `Object` and called `hashCode`/`equals` per probe
    // — both natives — so it measured the seam a second time and read 16x SLOWER than the native
    // map (and 3.75x slower on HotSpot too, i.e. the algorithm was simply worse). Keyed on `int`
    // it makes NO native call at all, which is the condition the question is actually about.
    static final class GuestMap {
        int[] k = new int[128];
        boolean[] used = new boolean[128];
        Object[] v = new Object[128];
        int mask = 127;
        int idx(int key) {
            int h = key & mask;
            while (used[h] && k[h] != key) h = (h + 1) & mask;
            return h;
        }
        void put(int key, Object val) { int h = idx(key); k[h] = key; used[h] = true; v[h] = val; }
        Object get(int key) { int h = idx(key); return used[h] ? v[h] : null; }
    }

    static java.util.HashMap<Object, Object> natMap = new java.util.HashMap<>();
    static GuestMap gstMap = new GuestMap();
    static Object[] keys = new Object[64];
    static int[] ikeys = new int[64];

    static java.util.ArrayList<Object> nat = new java.util.ArrayList<>();
    static GuestList gst = new GuestList();
    static Object sink;

    static long natGet(int reps) { long s=0; for(int r=0;r<reps;r++) for(int i=0;i<nat.size();i++) s += (Integer) nat.get(i); return s; }
    static long gstGet(int reps) { long s=0; for(int r=0;r<reps;r++) for(int i=0;i<gst.size();i++) s += (Integer) gst.get(i); return s; }

    static long natMapGet(int reps) { long s=0; for(int r=0;r<reps;r++) for(int i=0;i<keys.length;i++) s += (Integer) natMap.get(keys[i]); return s; }
    static long gstMapGet(int reps) { long s=0; for(int r=0;r<reps;r++) for(int i=0;i<ikeys.length;i++) s += (Integer) gstMap.get(ikeys[i]); return s; }

    static long timeMap(String tag, int reps, int elems, boolean guest) {
        long t0 = System.nanoTime();
        long s = guest ? gstMapGet(reps) : natMapGet(reps);
        long dt = System.nanoTime() - t0;
        sink = Long.valueOf(s);
        System.out.println(String.format("%-18s %6d ns/lookup", tag, dt / ((long) reps * elems)));
        return s;
    }

    static long time(String tag, int reps, int elems, boolean guest) {
        long t0 = System.nanoTime();
        long s = guest ? gstGet(reps) : natGet(reps);
        long dt = System.nanoTime() - t0;
        sink = Long.valueOf(s);
        long calls = (long) reps * elems;
        System.out.println(String.format("%-18s %6d ns/element", tag, dt / calls));
        return s;
    }

    public static void main(String[] args) {
        int elems = 64, reps = 20000;
        for (int i = 0; i < elems; i++) { Integer v = Integer.valueOf(i); nat.add(v); gst.add(v); }
        // warm both through the tiering threshold before either is timed
        natGet(2000); gstGet(2000);
        long a = time("list native", reps, elems, false);
        long b = time("list guest", reps, elems, true);
        if (a != b) throw new IllegalStateException("list arms disagree: " + a + " vs " + b);

        for (int i = 0; i < elems; i++) {
            keys[i] = Integer.valueOf(i * 7919);
            ikeys[i] = i * 7919;
            Integer val = Integer.valueOf(i);
            natMap.put(keys[i], val);
            gstMap.put(ikeys[i], val);
        }
        natMapGet(2000); gstMapGet(2000);
        long c = timeMap("map native", reps, elems, false);
        long d = timeMap("map guest", reps, elems, true);
        if (c != d) throw new IllegalStateException("map arms disagree: " + c + " vs " + d);
        System.out.println("checksum " + a + " " + c);
    }
}
