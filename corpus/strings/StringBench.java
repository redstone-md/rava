import java.util.*;
// Allocation- + toString-heavy workload. Order-INDEPENDENT accumulators so the
// checksum does not depend on HashMap iteration order (unspecified in Java).
public class StringBench {
    static final class Point {
        final int x, y; Point(int x, int y){this.x=x; this.y=y;}
        public String toString(){ return "(" + x + "," + y + ")"; }
    }
    public static void main(String[] args) {
        long lenSum = 0, hashXor = 0;
        List<Object> objs = new ArrayList<>();
        for (int i = 0; i < 30_000; i++) objs.add(new Point(i % 997, (i * 7) % 1013));
        for (int pass = 0; pass < 2; pass++)
            for (Object o : objs) { String s = String.valueOf(o); lenSum += s.length(); hashXor ^= s.hashCode(); }
        Map<String,Integer> m = new HashMap<>();
        for (int i = 0; i < 20_000; i++) { String k = new Point(i % 313, i % 331).toString(); m.merge(k, i, Integer::sum); }
        long keyLen = 0, valXor = 0;
        for (Map.Entry<String,Integer> e : m.entrySet()) { keyLen += e.getKey().length(); valXor ^= e.getValue().longValue(); }
        System.out.println("CHECKSUM=" + (lenSum ^ hashXor ^ keyLen ^ valXor));
        System.out.println("DONE");
    }
}
