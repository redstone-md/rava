import java.util.*;

// Bounds checks dropped for non-negative induction variables (opt/ivs.rs, conds.rs): an
// iterator's cursor indexes its list's array after its own length test, a hand-written loop
// tests the length itself, and the exceptions still come where they belong.
public class IvBoundsSem {
    static long walk(int[] a, int n) {
        long s = 0;
        int i = 0;
        while (i != n) {
            if (i >= a.length) throw new IllegalStateException("past the end at " + i);
            s += a[i] * 3L;
            i++;
        }
        return s;
    }

    static long iterate(List<Integer> list) {
        long s = 0;
        for (Integer x : list) s += x;
        return s;
    }

    public static void main(String[] args) {
        int[] a = new int[100];
        for (int i = 0; i < a.length; i++) a[i] = i * i - 50;
        List<Integer> list = new ArrayList<>();
        for (int i = 0; i < 300; i++) list.add(i * 7 - 1000);
        long h = 0;
        for (int r = 0; r < 20000; r++) {
            h = h * 31 + walk(a, r % 101);
            h = h * 31 + iterate(list);
        }
        System.out.println("hash " + h);
        try {
            walk(a, 150);
        } catch (IllegalStateException e) {
            System.out.println(e.getMessage());
        }
        try {
            for (Integer x : list) if (x > 0) list.remove(0);
        } catch (ConcurrentModificationException e) {
            System.out.println("cme " + list.size());
        }
        Iterator<Integer> it = list.iterator();
        long s = 0;
        while (it.hasNext()) s += it.next();
        try {
            it.next();
        } catch (NoSuchElementException e) {
            System.out.println("nsee " + s);
        }
        ListIterator<Integer> li = list.listIterator(list.size());
        long back = 0;
        while (li.hasPrevious()) back += li.previous();
        System.out.println("back " + back + " " + iterate(list));
    }
}
