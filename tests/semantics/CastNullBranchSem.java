// A cast whose result its block then tests for null: null goes straight to the branch's null
// side, which may still read the cast value (as null) through a phi; a non-null value of the
// wrong class still fails the cast; Unsafe-backed field reads at constant offsets (records'
// accessors behind their generated equals and hashCode) read the right fields.
import java.util.HashMap;

public class CastNullBranchSem {
    record Key(int a, long b, String c) {}
    static class Base {}
    static class Mid extends Base { int x = 3; }

    static int viaPhi(Object o, int k) {
        Integer v = (Integer) o;
        Object r;
        if (v == null) r = v; else r = v + k;
        return r == null ? -1 : (Integer) r;
    }

    static int classCast(Object o) {
        Mid m = (Mid) o;
        if (m != null) return m.x;
        return -7;
    }

    static int sink;
    static void touch(int k) { sink += k; }

    // The null side is never taken while profiling, so it deoptimizes; its state reads the cast
    // value (kept across the call in a home slot), which must hold null there.
    static int rare(Object o, int k) {
        Integer v = (Integer) o;
        touch(k);
        if (v == null) {
            touch(-k);
            return k + (v == null ? 1000 : 0);
        }
        touch(v);
        return v + k;
    }

    public static void main(String[] args) {
        long rareAcc = 0;
        for (int r = 0; r < 60000; r++) rareAcc += rare(r, r & 3);
        for (int r = 0; r < 100; r++) rareAcc += rare(null, r);
        System.out.println("rare " + rareAcc + " " + sink);
        Object[] vals = { 1, null, 5, null, 9 };
        Object[] mids = { new Mid(), null, new Mid() };
        HashMap<Key, Integer> map = new HashMap<>();
        for (int i = 0; i < 64; i++) map.put(new Key(i, i * 3L, "k" + (i % 5)), i);
        long acc = 0;
        for (int r = 0; r < 50000; r++) {
            acc += viaPhi(vals[r % 5], r & 7) + classCast(mids[r % 3]);
            Integer got = map.get(new Key(r & 63, (r & 63) * 3L, "k" + ((r & 63) % 5)));
            acc += got == null ? -100 : got;
            acc += new Key(r, r, null).hashCode() & 0xff;
        }
        System.out.println("acc " + acc);
        try {
            viaPhi("not an integer", 1);
        } catch (ClassCastException e) {
            System.out.println("cce");
        }
        try {
            classCast(new Base());
        } catch (ClassCastException e) {
            System.out.println("cce2");
        }
    }
}
