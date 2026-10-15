// A receiver that is a new object or null (a method returning an iterator, or null when there is
// nothing to iterate), inlined: calls on it dispatch to the allocation's class without a class
// test, and a null one still throws at the call.
public class NewOrNullSem {
    interface It { boolean hasNext(); int next(); }
    static final class ArrIt implements It {
        final int[] a; int i;
        ArrIt(int[] a) { this.a = a; }
        public boolean hasNext() { return i < a.length; }
        public int next() { return a[i++]; }
    }
    static final class Other implements It {
        public boolean hasNext() { return false; }
        public int next() { return -1; }
    }

    static It iterator(int[] a) { return a.length == 0 ? null : new ArrIt(a); }

    static long sum(int[] a) {
        It it = iterator(a);
        if (it == null) return -1;
        long s = 0;
        while (it.hasNext()) s += it.next();
        return s;
    }

    static long unchecked(int[] a) {
        It it = iterator(a);
        long s = 0;
        while (it.hasNext()) s += it.next() * 3;
        return s;
    }

    public static void main(String[] args) {
        int[][] arrays = {new int[0], {1, 2, 3}, {4, 5}, new int[17]};
        for (int i = 0; i < arrays[3].length; i++) arrays[3][i] = i * i;
        long s = 0, npes = 0;
        It other = new Other();
        for (int r = 0; r < 100000; r++) {
            for (int[] a : arrays) {
                s += sum(a);
                try {
                    s += unchecked(a);
                } catch (NullPointerException e) {
                    npes++;
                }
            }
            if (other.hasNext()) s++;
        }
        System.out.println(s + " " + npes);
    }
}
