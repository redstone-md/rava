/** Calls devirtualized by the receiver's exact class: default methods calling interface
 *  methods on `this`, several implementations sharing one default method, and new objects. */
public class OptDevirtSem {
    interface Height {
        int min();
        int height();
        default int max() { return min() + height(); }
        default int sections() { return (max() - min()) >> 4; }
        default boolean outside(int y) { return y < min() || y >= max(); }
    }
    static final class Proto implements Height {
        final int lo;
        Proto(int lo) { this.lo = lo; }
        public int min() { return lo; }
        public int height() { return 384; }
    }
    static final class Level implements Height {
        public int min() { return 0; }
        public int height() { return 256; }
        public int sections() { return 99; }   // overrides the default
    }
    static class Region implements Height {
        public int min() { return -32; }
        public int height() { return 128; }
    }
    static class SubRegion extends Region {
        public int height() { return 64; }
    }

    static int use(Height h, int y) {
        return h.max() * 3 + h.sections() * 7 + (h.outside(y) ? 1 : 0);
    }

    public static void main(String[] args) {
        Height[] hs = {new Proto(-64), new Level(), new Region(), new SubRegion(), new Proto(0)};
        long s = 0;
        for (int i = 0; i < 300000; i++) {
            Height h = hs[(i >> 3) % (i < 150000 ? 1 : hs.length)];   // monomorphic first, then not
            s += use(h, i % 500 - 100);
            s += new Proto(i & 15).max();
        }
        System.out.println(s);
    }
}
