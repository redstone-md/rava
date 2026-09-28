import java.util.stream.*;
// Minimal standalone reproducer for the IntStream pipeline divergence.
// Narrows the chain step by step. No benchmark harness.
public class StreamRepro {
    public static void main(String[] a) {
        int N = 20_000;
        // A: IntStream.range().sum()
        System.out.println("A_range_sum=" + IntStream.range(0, N).sum());
        // B: mapToObj(Integer::toString).count()
        System.out.println("B_mapToObj_count=" + IntStream.range(0, N).mapToObj(Integer::toString).count());
        // C: mapToObj -> filter(len>2) -> count
        System.out.println("C_filter_count=" + IntStream.range(0, N)
            .mapToObj(Integer::toString).filter(t -> t.length() > 2).count());
        // D: mapToObj -> mapToLong(hashCode) -> sum   (NO filter)
        System.out.println("D_mapToLong_sum=" + IntStream.range(0, N)
            .mapToObj(Integer::toString).mapToLong(t -> (long) t.hashCode()).sum());
        // E: full chain (the failing one)
        System.out.println("E_full=" + IntStream.range(0, N)
            .mapToObj(Integer::toString).filter(t -> t.length() > 2)
            .mapToLong(t -> (long) t.hashCode()).sum());
        // F: mapToObj -> map(identity) -> mapToLong  (probe mapToObj sink chaining)
        System.out.println("F_map_chain=" + IntStream.range(0, 5)
            .mapToObj(Integer::toString).map(s -> s + "!").collect(Collectors.joining(",")));
        // G: mapToObj -> mapToInt(length) -> sum
        System.out.println("G_mapToInt=" + IntStream.range(0, N)
            .mapToObj(Integer::toString).mapToInt(String::length).sum());
        System.out.println("DONE");
    }
}
