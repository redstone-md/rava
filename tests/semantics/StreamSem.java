// Differential probe: streams, Optional and records — the shapes this jar is written in.
//
// TWO LINES ARE EXPECTED TO DIVERGE from `java -Xint`, both architectural — a third
// means something broke:
//
//   stats       `average=2,666667` on a comma-decimal JVM. This engine formats as
//               Locale.ROOT throughout; see bench/StrNum.java for the same divergence.
//
//   peek.order  the JVM threads ONE ELEMENT through the whole pipeline at a time, so a
//               peek/forEach pair interleaves: p1f2p2f4p3f6. This engine materialises
//               each stage before starting the next, so it batches: p1p2p3f2f4f6. Only
//               a side effect inside a stage can observe the difference, and the results
//               are identical either way.
//
//   groupingBy.count  ⚠**THE DECLARED REASON IS GONE; A DIFFERENT ONE REMAINS.** This read
//               "the classifier returns `char`... this engine's value model has no char
//               variant, so a boxed char is an int and prints as `97`". That is fixed
//               (`crate::boxid` — the keys now print `a`/`b`/`c`/`d`, and bench/NumSem.java
//               and bench/EqualsSem.java no longer declare their halves of the same gap).
//               What still differs is the ORDER: `groupingBy` collects into a `HashMap`,
//               whose iteration order the JVM derives from the key hashes (for one-character
//               keys that comes out sorted, `{a=2, b=1, c=1, d=1}`) while this engine's
//               native map keeps INSERTION order (`{d=1, a=2, c=1, b=1}`). Same four
//               groupings, same four counts. That is an engine-wide property of the map
//               representation, not a stream or a boxing question, so it stays declared here
//               under its real cause rather than its old one.
// Machine-readable form of the three notes above, for bench\sem_all.ps1.
// DIVERGES: stats
// DIVERGES: peek.order
// DIVERGES: groupingBy.count
import java.util.*;
import java.util.stream.*;
public class StreamSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    record Point(int x, int y) {}
    record Named(String name, List<Integer> xs) {}
    public static void main(String[] a) {
        List<String> src = List.of("delta", "alpha", "charlie", "bravo", "alpha");

        // --- ordering, laziness, short circuit
        p("map.sorted", src.stream().map(String::toUpperCase).sorted().collect(Collectors.toList()));
        p("distinct", src.stream().distinct().toList());
        p("limit.skip", src.stream().sorted().skip(1).limit(2).toList());
        p("anyMatch", src.stream().anyMatch(s -> s.startsWith("b")) + " " + src.stream().allMatch(s -> s.length() > 3)
            + " " + src.stream().noneMatch(String::isEmpty));
        p("findFirst", src.stream().filter(s -> s.length() == 5).findFirst().orElse("-"));
        p("count", src.stream().filter(s -> s.contains("a")).count());
        p("reduce", src.stream().reduce("", (x, y) -> x + y.charAt(0)));
        p("reduce.identity", Stream.<String>of().reduce("Z", String::concat));
        p("flatMap", Stream.of(List.of(1, 2), List.of(3)).flatMap(List::stream).toList());
        p("iterate.limit", Stream.iterate(1, i -> i * 2).limit(5).toList());
        p("generate", Stream.generate(() -> 7).limit(3).toList());
        p("range", IntStream.range(0, 5).boxed().toList() + " " + IntStream.rangeClosed(1, 3).sum());
        p("stats", IntStream.of(3, 1, 4).summaryStatistics().toString());
        p("minmax", src.stream().min(Comparator.naturalOrder()).get() + " " + src.stream().max(Comparator.comparingInt(String::length)).get());
        p("peek.order", peekOrder());

        // --- Collectors
        p("joining", src.stream().collect(Collectors.joining(", ", "[", "]")));
        p("toSet.size", src.stream().collect(Collectors.toSet()).size());
        p("groupingBy", new TreeMap<>(src.stream().collect(Collectors.groupingBy(String::length))));
        p("groupingBy.count", new TreeMap<>(src.stream().collect(Collectors.groupingBy(s -> s.charAt(0), Collectors.counting()))));
        p("partitioning", src.stream().collect(Collectors.partitioningBy(s -> s.length() > 5)));
        p("toMap", new TreeMap<>(src.stream().distinct().collect(Collectors.toMap(s -> s, String::length))));
        try { src.stream().collect(Collectors.toMap(s -> s, String::length)); p("toMap.dup", "NO THROW"); }
        catch (Exception e) { p("toMap.dup", e.getClass().getName()); }
        p("mapping", src.stream().collect(Collectors.mapping(s -> s.substring(0, 1), Collectors.toSet())).size());
        p("averaging", src.stream().collect(Collectors.averagingInt(String::length)));
        p("summing", src.stream().collect(Collectors.summingInt(String::length)));

        // --- Optional
        Optional<String> some = Optional.of("v"), none = Optional.empty();
        p("opt", some.get() + " " + none.orElse("d") + " " + none.orElseGet(() -> "g") + " " + some.isPresent() + " " + none.isEmpty());
        p("opt.map", some.map(String::toUpperCase).get() + " " + none.map(String::toUpperCase).isPresent());
        p("opt.filter", some.filter(s -> s.equals("v")).isPresent() + " " + some.filter(s -> s.equals("x")).isPresent());
        p("opt.flatMap", some.flatMap(s -> Optional.of(s + "!")).get());
        p("opt.ofNullable", Optional.ofNullable(null).isPresent() + " " + Optional.ofNullable("y").get());
        p("opt.toString", some + " " + none);
        p("opt.stream", some.stream().toList() + " " + none.stream().toList());
        p("opt.equals", some.equals(Optional.of("v")) + " " + none.equals(Optional.empty()));
        try { none.orElseThrow(); p("opt.orElseThrow", "NO THROW"); }
        catch (Exception e) { p("opt.orElseThrow", e.getClass().getName()); }
        try { Optional.of(null); p("opt.ofNull", "NO THROW"); }
        catch (Exception e) { p("opt.ofNull", e.getClass().getName()); }

        // --- records: generated equals/hashCode/toString and accessors
        Point p1 = new Point(1, 2), p2 = new Point(1, 2), p3 = new Point(2, 1);
        p("rec.toString", p1.toString());
        p("rec.equals", p1.equals(p2) + " " + p1.equals(p3) + " " + p1.equals(null) + " " + p1.equals("x"));
        p("rec.hash", (p1.hashCode() == p2.hashCode()) + " " + (p1.hashCode() == p3.hashCode()));
        p("rec.accessors", p1.x() + " " + p1.y());
        p("rec.inSet", new HashSet<>(List.of(p1, p2, p3)).size());
        p("rec.asKey", new HashMap<>(Map.of(p1, "a")).get(p2));
        Named n1 = new Named("n", List.of(1, 2)), n2 = new Named("n", List.of(1, 2));
        p("rec.deep", n1.equals(n2) + " " + (n1.hashCode() == n2.hashCode()) + " " + n1);
        p("rec.sorted", Stream.of(p3, p1).sorted(Comparator.comparingInt(Point::x)).toList());
    }
    static String peekOrder() {
        StringBuilder b = new StringBuilder();
        Stream.of(1, 2, 3).peek(i -> b.append('p').append(i)).map(i -> i * 2).forEach(i -> b.append('f').append(i));
        return b.toString();
    }
}
