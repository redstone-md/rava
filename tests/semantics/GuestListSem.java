// Differential probe for the list surface the 20 existing `*Sem` probes do NOT cover, written
// because a guest-bytecode `java.util.ArrayList` passes all 20 and still stops MC's datapack
// load. Views, identity, fail-fast, and what the factories actually hand back.
import java.util.*;

// The factory classes this engine does not model — every `java.util` list is one class here, so
// `List.of` cannot answer `List12` and a collector result cannot answer `ListN`. Pre-existing:
// the DEFAULT engine diverges on exactly these lines too, with or without the override.
// DIVERGES: ofType
// DIVERGES: emptyType
// DIVERGES: collectedType
// DIVERGES: collectedAdd

public class GuestListSem {
    static void p(String k, Object v) { System.out.println(k + "=" + v); }
    static String thr(Runnable r) {
        try { r.run(); return "ok"; } catch (Throwable t) { return t.getClass().getSimpleName(); }
    }
    public static void main(String[] a) {
        List<String> base = new ArrayList<>(Arrays.asList("a", "b", "c", "d"));

        // subList must be a VIEW: writes through it reach the parent, and parent size changes.
        List<String> sub = base.subList(1, 3);
        p("sub", sub);
        sub.set(0, "B");
        p("afterSubSet.base", base);
        p("afterSubSet.sub", sub);
        p("subClear", thr(() -> base.subList(0, 1).clear()));
        p("afterSubClear.base", base);

        // unmodifiable must be a DISTINCT object, live over the source.
        List<String> src = new ArrayList<>(Arrays.asList("x", "y"));
        List<String> un = Collections.unmodifiableList(src);
        p("unSame", un == src);
        src.add("z");
        p("unLive", un);
        p("unAdd", thr(() -> un.add("w")));

        // Fail-fast: structural change during iteration.
        p("cme", thr(() -> {
            List<String> l = new ArrayList<>(Arrays.asList("1", "2", "3"));
            for (String s : l) { l.add("x"); }
        }));

        // What do the factories hand back, and are they distinguishable?
        p("ofType", List.of("q").getClass().getSimpleName());
        p("asListType", Arrays.asList("q").getClass().getSimpleName());
        p("emptyType", Collections.emptyList().getClass().getSimpleName());
        p("ofRandomAccess", List.of("q") instanceof RandomAccess);
        p("newRandomAccess", new ArrayList<String>() instanceof RandomAccess);

        // Ordering and content through the stream/collector round trip.
        List<String> collected = base.stream().filter(s -> !s.equals("d")).toList();
        p("collected", collected);
        p("collectedType", collected.getClass().getSimpleName());
        p("collectedAdd", thr(() -> collected.add("nope")));
        p("sorted", base.stream().sorted().toList());

        // Equality across implementations.
        List<String> lk = new LinkedList<>(base);
        p("linkedCopy", lk);
        p("linkedSize", lk.size());
        p("baseNow", base);
        p("eqLinked", base.equals(lk));
        p("eqLinkedRev", lk.equals(base));
        p("eqOf", List.of("a").equals(Arrays.asList("a")));
        p("hashMatch", List.of("a").hashCode() == Arrays.asList("a").hashCode());

        // Nested/self containment and nulls.
        List<String> withNull = new ArrayList<>();
        withNull.add(null); withNull.add("n");
        p("indexOfNull", withNull.indexOf(null));
        p("containsNull", withNull.contains(null));
        p("withNull", withNull);
        p("iterRemoveAll", thr(() -> {
            List<String> l = new ArrayList<>(Arrays.asList("1", "2"));
            Iterator<String> it = l.iterator();
            while (it.hasNext()) { it.next(); it.remove(); }
            if (!l.isEmpty()) throw new IllegalStateException("left " + l);
        }));
    }
}
