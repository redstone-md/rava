// The CHEAP end of the native cascade -- and the end the boot actually lives at. Row 175:
// ArrayList.size() is 66 ns and StringBuilder.length() is 718 ns, and optimising the 718 ns
// tail bought the boot nothing because its natives (Map.get, Iterator.hasNext, ArrayList.get,
// Map$Entry.getKey) are all served early. So profile the cheap one: 66 ns against a 0.67 ns
// guest call is 100x paid BEFORE any linear scan, i.e. the fixed bridge prologue.
import java.util.ArrayList;

public class ListOnly {
    static long loop(int n, ArrayList<Integer> l) {
        long t0 = System.nanoTime(); int s = 0;
        for (int i = 0; i < n; i++) s += l.size();
        long t1 = System.nanoTime(); if (s == -1) System.out.print(""); return t1 - t0;
    }
    public static void main(String[] a) {
        int n = a.length > 0 ? Integer.parseInt(a[0]) : 8000000;
        ArrayList<Integer> l = new ArrayList<>(); l.add(7);
        double v = 0;
        for (int r = 1; r <= 6; r++) v = loop(n, l) / (double) n;
        System.out.println("LIST_SIZE_NS=" + v);
    }
}
