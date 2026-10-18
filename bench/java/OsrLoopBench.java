import java.util.Optional;

/** A hot loop in main, run once and so compiled on-stack: an Optional chain per element after a
 *  warm-up loop. ns per element, per round. Code after a loop that only on-stack-replaced code
 *  ever left once looked never reached (its calls refused as cold, its exit pruned). */
public class OsrLoopBench {
    interface Holder { Object value(); }
    record H(Object v) implements Holder { public Object value() { return v; } }
    static final class Entry {
        Holder holder;
        Entry(Holder h) { holder = h; }
        Object get() { return Optional.ofNullable(holder).map(Holder::value).orElseThrow(() -> new IllegalStateException("unbound " + this)); }
    }
    static String pollute(Object o) { return Optional.ofNullable(o).map(String::valueOf).orElse("none"); }

    public static void main(String[] a) {
        Entry[] es = new Entry[64];
        for (int i = 0; i < es.length; i++) es[i] = new Entry(new H("v" + i));
        long sink = 0;
        for (int i = 0; i < 200000; i++) sink += pollute((i & 3) == 0 ? null : "x").length();
        double best = Double.MAX_VALUE;
        for (int round = 0; round < 30; round++) {
            long t0 = System.nanoTime();
            for (int i = 0; i < 2_000_000; i++) sink += es[i & 63].get().hashCode();
            best = Math.min(best, (System.nanoTime() - t0) / 2e6);
        }
        System.out.printf("best %.2f ns per element (%d)%n", best, sink & 1);
    }
}
