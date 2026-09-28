// The cases an adversarial review of this session's merges flagged.
//
// Each line is a claim about what HotSpot does that the review made and that
// rava may or may not honour. Running it on both engines is what decides —
// the reviewer's reading of the JDK is a hypothesis, `java -Xint` is the answer.
import java.util.*;

public class ReviewEx {
    record P(int x) {}

    static void p(String tag, java.util.function.Supplier<Object> s) {
        try { System.out.println(tag + " " + s.get()); }
        catch (Throwable t) { System.out.println(tag + " !" + t.getClass().getName()); }
    }

    public static void main(String[] a) {
        // --- anewarray with an ARRAY component (javac: anewarray [I) ------------
        p("arr2d-cast", () -> {
            Object o = new int[3][];
            int[][] x = (int[][]) o;
            return x.length;
        });
        p("arr2d-str", () -> {
            Object o = new String[2][];
            String[][] x = (String[][]) o;
            return x.length;
        });
        p("arr3d-cast", () -> {
            Object o = new int[2][][];
            int[][][] x = (int[][][]) o;
            return x.length;
        });
        p("arr2d-multi", () -> {
            Object o = new int[2][2];
            int[][] x = (int[][]) o;
            return x[0][0];
        });
        p("arr2d-name", () -> new int[1][].getClass().getName() + " " + new int[1][1].getClass().getName());
        p("arr2d-store", () -> {
            Object[] holder = new int[2][];
            holder[0] = new int[3];
            return ((int[]) holder[0]).length;
        });
        p("arr-reflect", () -> java.lang.reflect.Array.newInstance(int[].class, 2).getClass().getName());

        // --- Set.of is a Set ------------------------------------------------------
        p("setof-eq-hashset", () -> Set.of("a").equals(new HashSet<>(List.of("a"))));
        p("hashset-eq-setof", () -> new HashSet<>(List.of("a")).equals(Set.of("a")));
        p("setof-eq-order", () -> Set.of("a", "b").equals(new LinkedHashSet<>(List.of("b", "a"))));
        p("setof-hash", () -> Set.of("a", "b").hashCode() == new HashSet<>(List.of("b", "a")).hashCode());
        p("setof-vs-list", () -> Set.of("a").equals(List.of("a")));
        p("emptyset-eq", () -> Collections.emptySet().equals(new HashSet<>()));
        p("singleton-eq", () -> Collections.singleton("a").equals(Set.of("a")));
        p("setof-dup-record", () -> Set.of(new P(1), new P(1)));
        p("setcopyof-dup-record", () -> Set.copyOf(Arrays.asList(new P(1), new P(1))).size());

        // --- views of an unmodifiable map -----------------------------------------
        p("mapof-keyset-remove", () -> Map.of("k", "v").keySet().remove("k"));
        p("mapof-values-remove", () -> Map.of("k", "v").values().remove("v"));
        p("mapof-entryset-clear", () -> { Map.of("k", "v").entrySet().clear(); return "no-throw"; });
        p("mapof-setvalue", () -> {
            for (Map.Entry<String, String> e : Map.of("k", "v").entrySet()) e.setValue("z");
            return "no-throw";
        });
        p("unmodmap-keyset-remove", () -> Collections.unmodifiableMap(new HashMap<>(Map.of("k", "v"))).keySet().remove("k"));
        p("emptymap-keyset-remove", () -> Collections.emptyMap().keySet().remove("k"));
        p("mapof-keyset-iter-remove", () -> {
            Iterator<String> it = Map.of("k", "v").keySet().iterator();
            it.next(); it.remove();
            return "no-throw";
        });
        p("listof-iter-remove", () -> {
            Iterator<String> it = List.of("a").iterator();
            it.next(); it.remove();
            return "no-throw";
        });
        p("listof-sublist-add", () -> List.of("a", "b").subList(0, 1).add("z"));
        p("listof-sublist-read", () -> List.of("a", "b").subList(0, 1).toString());

        // --- the singleton overrides ------------------------------------------------
        p("singletonlist-sort", () -> { Collections.singletonList("a").sort(null); return "no-throw"; });
        p("emptylist-sort", () -> { Collections.<String>emptyList().sort(null); return "no-throw"; });
        p("singletonlist-removeIf-false", () -> Collections.singletonList("a").removeIf(x -> false));
        p("singleton-removeIf-false", () -> Collections.singleton("a").removeIf(x -> false));
        p("ncopies-removeIf-false", () -> Collections.nCopies(2, "x").removeIf(x -> false));
        p("arraysaslist-set", () -> { List<String> l = Arrays.asList("a", "b"); l.set(0, "z"); return l.toString(); });
        p("arraysaslist-sort", () -> { List<String> l = Arrays.asList("b", "a"); l.sort(null); return l.toString(); });
        p("arraysaslist-add", () -> Arrays.asList("a").add("b"));

        // --- self-referential structures --------------------------------------------
        p("self-list-contains", () -> {
            List<Object> l = new ArrayList<>();
            l.add(l);
            return l.contains("x");
        });
        p("self-array-deepequals", () -> {
            Object[] x = new Object[1];
            x[0] = x;
            return Arrays.deepEquals(x, x);
        });
        p("self-array-deephash", () -> {
            Object[] x = new Object[1];
            x[0] = new int[]{1, 2};
            return Arrays.deepHashCode(x) == Arrays.deepHashCode(new Object[]{new int[]{1, 2}});
        });

        // --- format positional + missing argument --------------------------------------
        p("fmt-positional", () -> String.format("%2$s %1$s", "a", "b"));
        p("fmt-positional-reuse", () -> String.format("%1$s %1$s", "a"));
        p("fmt-positional-then-rel", () -> String.format("%1$s %s", "a", "b"));
        p("fmt-paren-zero", () -> String.format("%(05d", -42));
        p("fmt-nl-no-arg", () -> String.format("%s%n", "a").trim());

        // --- Arrays range overloads ---------------------------------------------------
        p("arr-equals-range", () -> Arrays.equals(new int[]{1, 2, 3}, 0, 2, new int[]{1, 2, 9}, 0, 2));
        p("arr-equals-range-false", () -> Arrays.equals(new int[]{1, 2}, 0, 2, new int[]{9, 9}, 0, 2));

        // --- Optional flavours -----------------------------------------------------------
        p("optint-tostring", () -> OptionalInt.of(3).toString() + " " + OptionalInt.empty()
            + " " + OptionalLong.of(4) + " " + OptionalDouble.empty());
    }
}
