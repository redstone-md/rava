// Differential probe: MAP VIEWS and the mutation contracts around them.
//
// WHY THIS AREA. `keySet()`, `values()` and `entrySet()` are specified as LIVE VIEWS of the
// map: removing through them removes from the map, `Entry.setValue` writes through, and the
// view's size follows the map's. The list views (`subList`, `Arrays.asList`) turned out to
// be COPIES in this engine, which is a silently wrong answer — every read is correct and
// only the write-back is missing — so the map views are the obvious next place to look.
//
// It also covers the mutators whose contracts are easy to get subtly wrong: compute /
// merge / putIfAbsent return the NEW or the OLD value depending on which one, and
// `computeIfAbsent` must not insert when the function returns null.
//
// Pure JDK, no Minecraft jar, seconds to run. Every line prints an observable.
import java.util.*;
import java.util.stream.*;

public class MapViewSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }

    static String ex(Runnable r) {
        try { r.run(); return "NO THROW"; }
        catch (Throwable t) { return t.getClass().getName(); }
    }

    static Map<String, Integer> m() {
        Map<String, Integer> m = new LinkedHashMap<>();
        m.put("a", 1);
        m.put("b", 2);
        m.put("c", 3);
        return m;
    }

    public static void main(String[] args) {
        // --- the views are VIEWS ---------------------------------------------------
        Map<String, Integer> m = m();
        Set<String> ks = m.keySet();
        Collection<Integer> vs = m.values();
        Set<Map.Entry<String, Integer>> es = m.entrySet();
        p("keySet", ks);
        p("values", vs);
        p("entrySet", es);
        m.put("d", 4);
        p("keySet.live", ks);
        p("values.live", vs);
        p("entrySet.live", es);
        p("keySet.sizeLive", ks.size());

        // removal THROUGH the view removes from the map
        Map<String, Integer> m2 = m();
        m2.keySet().remove("a");
        p("keySet.remove", m2);
        Map<String, Integer> m3 = m();
        m3.values().remove(2);
        p("values.remove", m3);
        Map<String, Integer> m4 = m();
        m4.keySet().removeIf(k -> k.equals("c"));
        p("keySet.removeIf", m4);
        Map<String, Integer> m5 = m();
        m5.entrySet().removeIf(e -> e.getValue() == 2);
        p("entrySet.removeIf", m5);
        Map<String, Integer> m6 = m();
        m6.keySet().retainAll(List.of("b"));
        p("keySet.retainAll", m6);
        Map<String, Integer> m7 = m();
        m7.values().clear();
        p("values.clear", m7);

        // Entry.setValue writes THROUGH
        Map<String, Integer> m8 = m();
        for (Map.Entry<String, Integer> e : m8.entrySet()) {
            if (e.getKey().equals("b")) { e.setValue(20); }
        }
        p("entry.setValue", m8);

        // views REFUSE addition
        p("keySet.add", ex(() -> m().keySet().add("z")));
        p("values.add", ex(() -> m().values().add(9)));

        // --- containment and equality of the views ---------------------------------
        p("keySet.contains", m().keySet().contains("b"));
        p("values.contains", m().values().contains(3));
        p("keySet.eq", m().keySet().equals(Set.of("a", "b", "c")));
        p("keySet.hashEq", m().keySet().hashCode() == Set.of("a", "b", "c").hashCode());
        p("entry.eq", m().entrySet().iterator().next().equals(Map.entry("a", 1)));

        // --- mutator return values --------------------------------------------------
        Map<String, Integer> c = m();
        p("put.old", c.put("a", 10));
        p("put.new", c.put("z", 1));
        p("remove.hit", c.remove("b"));
        p("remove.miss", c.remove("nope"));
        p("putIfAbsent.present", c.putIfAbsent("a", 99));
        p("putIfAbsent.absent", c.putIfAbsent("q", 7));
        p("getOrDefault", c.getOrDefault("nope", -1));
        p("replace.present", c.replace("a", 11));
        p("replace.absent", c.replace("nope", 1));
        p("computeIfAbsent.new", c.computeIfAbsent("ci", k -> 5));
        p("computeIfAbsent.existing", c.computeIfAbsent("ci", k -> 6));
        p("computeIfAbsent.null", c.computeIfAbsent("nul", k -> null));
        p("computeIfAbsent.notInserted", c.containsKey("nul"));
        p("computeIfPresent.hit", c.computeIfPresent("ci", (k, v) -> v + 1));
        p("computeIfPresent.miss", c.computeIfPresent("nope", (k, v) -> 1));
        p("compute.toNull", c.compute("ci", (k, v) -> null));
        p("compute.removed", c.containsKey("ci"));
        p("merge.absent", c.merge("mg", 1, Integer::sum));
        p("merge.present", c.merge("mg", 2, Integer::sum));
        p("merge.toNull", c.merge("mg", 0, (x, y) -> null));
        p("merge.removed", c.containsKey("mg"));
        p("final", new TreeMap<>(c));

        // --- iteration order and streams over the views ------------------------------
        p("keySet.stream", m().keySet().stream().collect(Collectors.joining(",")));
        p("values.stream", m().values().stream().map(String::valueOf).collect(Collectors.joining(",")));
        p("entry.stream", m().entrySet().stream()
                .map(e -> e.getKey() + "=" + e.getValue()).collect(Collectors.joining(",")));
        p("forEach", forEachOrder());

        // --- unmodifiable wrappers refuse ---------------------------------------------
        Map<String, Integer> um = Collections.unmodifiableMap(m());
        p("unmod.put", ex(() -> um.put("x", 1)));
        p("unmod.keySetRemove", ex(() -> um.keySet().remove("a")));
        p("unmod.read", um.get("a"));
        p("mapOf.put", ex(() -> Map.of("a", 1).put("b", 2)));
        p("mapOf.keySetRemove", ex(() -> Map.of("a", 1).keySet().remove("a")));
        p("mapCopyOf", Map.copyOf(m()).get("b"));

        // --- null handling differs BY MAP KIND ----------------------------------------
        Map<String, Integer> hm = new HashMap<>();
        hm.put(null, 1);
        p("hashMap.nullKey", hm.get(null));
        hm.put("k", null);
        p("hashMap.nullValue", hm.get("k"));
        p("hashMap.containsNullValue", hm.containsValue(null));
        p("treeMap.nullKey", ex(() -> new TreeMap<String, Integer>().put(null, 1)));
        p("mapOf.nullKey", ex(() -> Map.of(null, 1)));
    }

    static String forEachOrder() {
        StringBuilder b = new StringBuilder();
        m().forEach((k, v) -> b.append(k).append(v));
        return b.toString();
    }
}
