import java.util.*;

// Universal integer compute + collections + hashmap + string-build workload.
// Bit-exact deterministic across any correct JVM. (Stream form excluded: RAVA
// IntStream.mapToObj->filter->mapToLong->sum returns 0 — tracked separately.)
public class ComputeBench {
    public static void main(String[] args) {
        long acc = 0, h = 1125899906842597L;
        for (int i = 1; i <= 300_000; i++) { h = h * 31 + (i % 7); acc += h ^ (i * 2654435761L); }
        List<Integer> xs = new ArrayList<>();
        Random r = new Random(42);
        for (int i = 0; i < 40_000; i++) xs.add(r.nextInt(1_000_000));
        Collections.sort(xs);
        for (int x : xs) acc += x;
        Map<String, Integer> m = new HashMap<>();
        for (int i = 0; i < 20_000; i++) { String k = "k" + (i % 2000); m.merge(k, i, Integer::sum); }
        for (Map.Entry<String, Integer> e : m.entrySet()) acc += e.getValue().longValue();
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 10_000; i++) sb.append(i).append(',');
        acc += sb.length();
        acc ^= String.valueOf(sb.toString().hashCode()).hashCode();
        System.out.println("CHECKSUM=" + acc);
        System.out.println("DONE");
    }
}
