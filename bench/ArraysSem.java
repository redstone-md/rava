// Differential probe: java.util.Arrays and java.util.Collections — sorting, searching,
// filling and the wrapper factories. Used everywhere; probed until now only in passing.
import java.util.*;
public class ArraysSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    static String ex(Runnable r) {
        try { r.run(); return "NO THROW"; }
        catch (Throwable t) { return t.getClass().getName(); }
    }
    record Person(String name, int age) {}
    public static void main(String[] a) {
        // --- primitive sorts and searches
        int[] xs = { 5, 3, 9, 1, 3 };
        int[] ys = xs.clone();
        Arrays.sort(ys);
        p("sort.int", Arrays.toString(ys) + " " + Arrays.binarySearch(ys, 5) + " " + Arrays.binarySearch(ys, 4));
        double[] ds = { 2.5, Double.NaN, -0.0, 0.0, 1.0 };
        Arrays.sort(ds);
        p("sort.double", Arrays.toString(ds));
        String[] ss = { "b", "A", "a", "B" };
        Arrays.sort(ss);
        p("sort.string", Arrays.toString(ss));
        String[] ci = { "b", "A", "a", "B" };
        Arrays.sort(ci, String.CASE_INSENSITIVE_ORDER);
        p("sort.ci", Arrays.toString(ci));

        // --- sort is STABLE for objects
        Person[] ps = { new Person("x", 2), new Person("y", 1), new Person("z", 2), new Person("w", 1) };
        Arrays.sort(ps, Comparator.comparingInt(Person::age));
        StringBuilder b = new StringBuilder();
        for (Person q : ps) b.append(q.name());
        p("sort.stable", b.toString());

        // --- range sorts and fills
        int[] r = { 9, 8, 7, 6, 5 };
        Arrays.sort(r, 1, 4);
        p("sort.range", Arrays.toString(r));
        int[] f = new int[5];
        Arrays.fill(f, 3);
        Arrays.fill(f, 1, 3, 7);
        p("fill", Arrays.toString(f));
        int[] sa = new int[5];
        Arrays.setAll(sa, i -> i * i);
        p("setAll", Arrays.toString(sa));

        // --- copying, comparing, hashing
        p("copy", Arrays.toString(Arrays.copyOf(xs, 7)) + " " + Arrays.toString(Arrays.copyOfRange(xs, 1, 3)));
        p("equals", Arrays.equals(xs, xs.clone()) + " " + Arrays.equals(new int[0], new int[0])
            + " " + Arrays.hashCode(new int[]{1,2}) + " " + Arrays.hashCode((int[]) null));
        int[][] deep = { {1,2}, {3} };
        p("deep", Arrays.deepToString(deep) + " " + Arrays.deepEquals(deep, new int[][]{{1,2},{3}})
            + " " + (Arrays.deepHashCode(deep) == Arrays.deepHashCode(new int[][]{{1,2},{3}})));
        p("mismatch", Arrays.mismatch(new int[]{1,2,3}, new int[]{1,2,4})
            + " " + Arrays.compare(new int[]{1,2}, new int[]{1,3}));
        p("stream", Arrays.stream(xs).sum() + " " + Arrays.stream(ss).count());

        // --- errors
        p("sort.null", ex(() -> Arrays.sort((int[]) null)));
        p("copy.neg", ex(() -> Arrays.copyOf(xs, -1)));
        p("range.bad", ex(() -> Arrays.copyOfRange(xs, 3, 1)));

        // --- Collections: order-changing helpers
        List<Integer> li = new ArrayList<>(List.of(1, 2, 3, 4));
        Collections.reverse(li);
        p("reverse", li);
        Collections.swap(li, 0, 3);
        p("swap", li);
        Collections.rotate(li, 1);
        p("rotate", li);
        p("minmax", Collections.min(li) + " " + Collections.max(li)
            + " " + Collections.frequency(li, 2) + " " + Collections.disjoint(li, List.of(99)));
        Collections.sort(li);
        p("sort", li + " " + Collections.binarySearch(li, 3));
        Collections.fill(li, 0);
        p("fill", li);

        // --- Collections: the factories
        p("nCopies", Collections.nCopies(3, "x") + " " + Collections.emptyList() + " " + Collections.singletonList(7));
        p("singleton", Collections.singleton("s") + " " + Collections.singletonMap("k", "v"));
        p("addAll", addAll());
        List<Integer> um = Collections.unmodifiableList(new ArrayList<>(List.of(1, 2)));
        p("unmodifiable", um + " " + ex(() -> um.add(3)));
        p("emptyImmutable", ex(() -> Collections.emptyList().add(1)));
        p("singletonImmutable", ex(() -> Collections.singletonList(1).set(0, 2)));
        List<Integer> sync = Collections.synchronizedList(new ArrayList<>(List.of(4)));
        sync.add(5);
        p("synchronized", sync);
        p("minEmpty", ex(() -> Collections.min(new ArrayList<Integer>())));
    }
    static String addAll() {
        List<String> l = new ArrayList<>();
        Collections.addAll(l, "a", "b");
        return l.toString();
    }
}
