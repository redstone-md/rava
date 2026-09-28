import java.util.*;
import java.util.stream.*;
public class Bisect {
    public static void main(String[] a) {
        long acc = 0, h = 1125899906842597L;
        for (int i = 1; i <= 300_000; i++) { h = h * 31 + (i % 7); acc += h ^ (i * 2654435761L); }
        System.out.println("S1=" + acc);
        List<Integer> xs = new ArrayList<>(); Random r = new Random(42);
        for (int i = 0; i < 40_000; i++) xs.add(r.nextInt(1_000_000));
        long ssum = 0; for (int x : xs) ssum += x;
        System.out.println("S2_randsum=" + ssum);
        Collections.sort(xs); long sorted = 0; for (int x : xs) sorted += x;
        System.out.println("S2_sortsum=" + sorted);
        Map<String,Integer> m = new HashMap<>();
        for (int i = 0; i < 20_000; i++) { String k = "k" + (i % 2000); m.merge(k, i, Integer::sum); }
        long msum = 0; for (Map.Entry<String,Integer> e : m.entrySet()) msum += e.getValue().longValue();
        System.out.println("S3_mapsum=" + msum);
        long s = IntStream.range(0, 20_000).mapToObj(Integer::toString)
            .filter(t -> t.length() > 2).mapToLong(t -> (long) t.hashCode()).sum();
        System.out.println("S4_stream=" + s);
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 10_000; i++) sb.append(i).append(',');
        System.out.println("S5_sblen=" + sb.length() + " S5_hash=" + sb.toString().hashCode());
    }
}
