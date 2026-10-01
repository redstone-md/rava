// Differential probe: ORDER of sorted collections, against `java -Xint`.
//
// Why it exists: rava's `ImmutableSortedMap.copyOf` materialised the copy in SOURCE order, and
// Minecraft's StateDefinition iterates exactly that map to build the cartesian product of block
// states -- a state's index in that list is its protocol id, so 238 of 1,166 blocks had wire-
// visible ids that disagreed with the JVM. Every earlier probe inserted keys already sorted and
// could not see it; this one inserts in reverse and in hash order, and prints every view that a
// sorted map exposes (keySet, values, entrySet, forEach, stream, firstKey/lastKey).
import java.util.*;
import java.util.stream.*;
import com.google.common.collect.*;

public class SortedSem {
    static void line(String k, Object v) { System.out.println(k + " " + v); }

    public static void main(String[] a) {
        List<String> rev = List.of("waterlogged", "stage", "hanging", "age");
        Map<String, Integer> src = new HashMap<>();
        for (String s : rev) src.put(s, s.length());

        ImmutableSortedMap<String, Integer> m = ImmutableSortedMap.copyOf(src);
        line("ism.keySet", m.keySet());
        line("ism.values", m.values());
        line("ism.entrySet", m.entrySet());
        List<String> fe = new ArrayList<>(); m.forEach((k, v) -> fe.add(k + "=" + v));
        line("ism.forEach", fe);
        line("ism.stream", m.values().stream().map(String::valueOf).collect(Collectors.joining(",")));
        line("ism.first.last", m.firstKey() + " " + m.lastKey());
        line("ism.copyOfDesc", ImmutableSortedMap.copyOf(new TreeMap<>(src).descendingMap()).keySet());
        ImmutableSortedMap.Builder<String, Integer> b = ImmutableSortedMap.naturalOrder();
        for (String s : rev) b.put(s, 1);
        line("ism.builder", b.build().keySet());
        line("ism.of", ImmutableSortedMap.of("b", 1, "a", 2, "c", 3).keySet());

        line("iss.copyOf", ImmutableSortedSet.copyOf(rev));
        line("iss.of", ImmutableSortedSet.of(3, 1, 2));
        line("treemap", new TreeMap<>(src).keySet() + " " + new TreeMap<>(src).values());
        line("treeset", new TreeSet<>(rev));
        line("im.copyOf", ImmutableMap.copyOf(new LinkedHashMap<>(Map.of("only", 1))).keySet());

        // The exact shape StateDefinition uses: cartesian product over the sorted map's values.
        Map<String, List<String>> props = new HashMap<>();
        props.put("waterlogged", List.of("true", "false"));
        props.put("hanging", List.of("true", "false"));
        props.put("age", List.of("0", "1"));
        ImmutableSortedMap<String, List<String>> sorted = ImmutableSortedMap.copyOf(props);
        Stream<List<String>> st = Stream.of(Collections.emptyList());
        for (List<String> vals : sorted.values()) {
            st = st.flatMap(prefix -> vals.stream().map(v -> { List<String> l = new ArrayList<>(prefix); l.add(v); return l; }));
        }
        List<String> out = new ArrayList<>();
        st.forEach(l -> out.add(String.join("/", l)));
        line("cartesian", out);
    }
}
