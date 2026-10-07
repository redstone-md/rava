// Reference array stores from hot code: anything into an Object[], subclasses and interface
// implementations into arrays of their supertypes, and an ArrayStoreException for a store into
// an array whose runtime element type is narrower than its static one (String[] seen as
// Object[], Integer[] seen as Number[]).
public class AaStoreSem {
    static int store(Object[] a, Object v, int i) {
        try {
            a[i] = v;
            return 1;
        } catch (ArrayStoreException e) {
            return 100;
        }
    }

    public static void main(String[] args) {
        Object[] plain = new Object[8];
        Object[] strings = new String[8];
        Number[] ints = new Integer[8];
        CharSequence[] seqs = new CharSequence[8];
        Object[] values = { "a", 1, 2.5, new StringBuilder("b"), new int[1], null };
        long s = 0;
        for (int r = 0; r < 200_000; r++) {
            Object v = values[r % values.length];
            int i = r & 7;
            s += store(plain, v, i);
            s += store(strings, v, i) * 3;
            s += store(ints, v instanceof Number n ? n : null, i) * 5;
            if (v instanceof CharSequence c) s += store(seqs, c, i) * 7;
            if (r % 1000 == 999) s += store(ints, 4.5, i) * 11;
        }
        System.out.println(s + " " + plain[3] + " " + strings[0] + " " + ints[1] + " " + seqs[3]);
    }
}
