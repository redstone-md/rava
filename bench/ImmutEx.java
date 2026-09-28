// The JDK's unmodifiable / fixed-size collection contracts.
//
// Two families that are NOT the same: `ImmutableCollections` (List.of, Set.of,
// Map.of, *.copyOf, Collections.unmodifiable*) throws from EVERY mutator whatever
// the contents; the AbstractList-backed singletons (emptyList, singletonList,
// nCopies, Arrays.asList) inherit default bodies that only throw when the
// operation would really mutate. Diff against `java -Xint`.
import java.util.*;

public class ImmutEx {
    static void p(String tag, Runnable r) {
        try { r.run(); System.out.println(tag + " ok"); }
        catch (Throwable t) { System.out.println(tag + " " + t.getClass().getName()); }
    }

    public static void main(String[] a) {
        p("of-add", () -> List.of("a").add("b"));
        p("of-remove-absent", () -> List.of("a").remove("zz"));
        p("of-clear", () -> List.of("a").clear());
        p("of-set", () -> List.of("a").set(0, "b"));
        p("of-sort", () -> List.<String>of().sort(null));
        p("of-removeIf-false", () -> List.of("a").removeIf(x -> false));
        p("of-addAll-empty", () -> List.of("a").addAll(List.of()));
        p("setof-add", () -> Set.of("a").add("b"));
        p("mapof-put", () -> Map.of("k", "v").put("k2", "v2"));
        p("mapof-remove-absent", () -> Map.of("k", "v").remove("zz"));
        p("copyOf-add", () -> List.copyOf(new ArrayList<>(List.of("a"))).add("b"));

        p("unmod-add", () -> Collections.unmodifiableList(new ArrayList<>(List.of("a"))).add("b"));
        p("unmodmap-put", () -> Collections.unmodifiableMap(new HashMap<>()).put("k", "v"));

        p("empty-add", () -> Collections.emptyList().add("x"));
        p("empty-removeIf", () -> Collections.emptyList().removeIf(x -> true));
        p("empty-clear", () -> Collections.emptyList().clear());
        p("empty-sort", () -> Collections.<String>emptyList().sort(null));
        p("singleton-set", () -> Collections.singletonList("a").set(0, "b"));
        p("singleton-add", () -> Collections.singletonList("a").add("b"));
        p("ncopies-add", () -> Collections.nCopies(2, "x").add("y"));
        p("emptymap-put", () -> Collections.emptyMap().put("k", "v"));
        p("emptymap-remove", () -> Collections.emptyMap().remove("k"));

        p("collections-sort-of", () -> Collections.sort(List.of("b", "a")));
        p("collections-sort-empty-of", () -> Collections.sort(List.<String>of()));
        p("collections-addAll-of", () -> Collections.addAll(List.of("a"), "b"));
        p("collections-addAll-none", () -> Collections.addAll(List.of("a")));
        p("collections-reverse-of", () -> Collections.reverse(List.of("a", "b")));

        p("of-null", () -> List.of((String) null));
        p("setof-dup", () -> Set.of("a", "a"));
        p("mapof-null-value", () -> Map.of("k", null));
        p("mapof-dup-key", () -> Map.of("k", "1", "k", "2"));
        p("entry-null", () -> Map.entry("k", null));

        System.out.println("setcopyof-dedup " + Set.copyOf(Arrays.asList("a", "a", "b")).size());
        List<String> mutable = new ArrayList<>(List.of("a"));
        mutable.add("b");
        System.out.println("mutable-still-works " + mutable);
        List<String> copy = new ArrayList<>(List.of("x", "y"));
        copy.remove("x");
        System.out.println("copy-of-immutable-is-mutable " + copy);
        System.out.println("unmod-view-live " + liveView());
    }

    static String liveView() {
        List<String> src = new ArrayList<>(List.of("a"));
        List<String> view = Collections.unmodifiableList(src);
        src.add("b");
        return view.toString() + " size=" + view.size();
    }
}
