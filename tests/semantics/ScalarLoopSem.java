// Scalar replacement of objects whose fields change in loops: a deoptimization mid-loop must
// rebuild each object with the field values of that moment.
public class ScalarLoopSem {
    static final class Cursor {
        int pos;
        long sum;
        byte tag;
        boolean odd;
        char ch;
        short sh;
        Object last;
    }

    interface Step { int next(int x); }
    static final class Inc implements Step { public int next(int x) { return x + 1; } }
    static final class Dbl implements Step { public int next(int x) { return x * 2 + 1; } }

    static Cursor seen;

    // `step` is monomorphic while warming up; a new receiver class later deoptimizes inside the
    // loop, where `c` exists only as scalars.
    static long walk(int[] a, Step step, boolean leak) {
        Cursor c = new Cursor();
        while (c.pos < a.length) {
            int v = step.next(a[c.pos]);
            c.sum += v;
            c.tag = (byte) (c.tag + v);
            c.odd = (v & 1) != 0;
            c.ch = (char) (c.ch + 3);
            c.sh = (short) (c.sh - v);
            if (c.pos == 7) c.last = "seven";
            c.pos++;
        }
        if (leak) seen = c;
        return c.sum * 31 + c.tag * 7 + (c.odd ? 1 : 0) + c.ch + c.sh + (c.last == null ? 0 : c.last.hashCode());
    }

    static long iterate(java.util.List<Integer> l) {
        long s = 0;
        for (Integer x : l) s += x;
        return s;
    }

    public static void main(String[] args) {
        int[] a = new int[100];
        for (int i = 0; i < a.length; i++) a[i] = i * 37 % 101;
        long acc = 0;
        for (int r = 0; r < 20000; r++) acc += walk(a, new Inc(), false);
        System.out.println("warm " + acc);
        System.out.println("other " + walk(a, new Dbl(), false));
        System.out.println("again " + walk(a, new Inc(), true));
        System.out.println("fields " + seen.pos + " " + seen.sum + " " + seen.tag + " " + seen.odd + " " + (int) seen.ch + " " + seen.sh + " " + seen.last);
        java.util.List<Integer> l = new java.util.ArrayList<>();
        for (int i = 0; i < 1000; i++) l.add(i);
        long t = 0;
        for (int r = 0; r < 3000; r++) t += iterate(l);
        System.out.println("list " + t);
        // A structural change mid-iteration throws from inside the replaced iterator.
        try {
            for (Integer x : l) if (x == 500) l.add(-1);
        } catch (java.util.ConcurrentModificationException e) {
            System.out.println("cme");
        }
        System.out.println("linked " + iterate(new java.util.LinkedList<>(l)));
    }
}
