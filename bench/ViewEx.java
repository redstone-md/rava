// keySet()/values()/entrySet() are LIVE VIEWS, and collections hash by CONTENT.
//
// Every line below is a contract a snapshot-returning native gets wrong in a way
// no gate hash can see: the map keeps stale entries, or two equal collections
// disagree on equals/hashCode. Diff against `java -Xint`.
import java.util.*;

public class ViewEx {
    static Map<String, Integer> m() {
        Map<String, Integer> m = new LinkedHashMap<>();
        m.put("a", 1); m.put("b", 2); m.put("c", 3);
        return m;
    }

    public static void main(String[] args) {
        Map<String, Integer> m = m();
        m.keySet().remove("a");
        System.out.println("keySet.remove " + m);

        m = m();
        m.values().remove(2);
        System.out.println("values.remove " + m);

        m = m();
        for (Iterator<Map.Entry<String, Integer>> it = m.entrySet().iterator(); it.hasNext(); ) {
            if (it.next().getValue() == 2) it.remove();
        }
        System.out.println("entrySet.iter.remove " + m);

        m = m();
        for (Map.Entry<String, Integer> e : m.entrySet()) e.setValue(e.getValue() * 10);
        System.out.println("setValue " + m);

        m = m();
        m.keySet().removeIf(k -> k.equals("b"));
        System.out.println("keySet.removeIf " + m);

        m = m();
        m.keySet().retainAll(List.of("a", "c"));
        System.out.println("keySet.retainAll " + m);

        m = m();
        m.keySet().removeAll(List.of("a", "c"));
        System.out.println("keySet.removeAll " + m);

        m = m();
        m.entrySet().clear();
        System.out.println("entrySet.clear " + m + " empty=" + m.isEmpty());

        Map<String, Integer> x = m(), y = m();
        System.out.println("map.equals " + x.equals(y) + " hashEq=" + (x.hashCode() == y.hashCode()));
        System.out.println("map.equals-diff " + x.equals(Map.of("a", 1)));
        System.out.println("map.equals-nonmap " + x.equals("a"));
        System.out.println("entrySet.equals " + x.entrySet().equals(y.entrySet()));
        System.out.println("keySet.equals " + x.keySet().equals(y.keySet())
            + " hashEq=" + (x.keySet().hashCode() == y.keySet().hashCode()));

        List<String> l1 = new ArrayList<>(List.of("p", "q"));
        List<String> l2 = new ArrayList<>(List.of("p", "q"));
        System.out.println("list.hash " + (l1.hashCode() == l2.hashCode()) + " " + l1.hashCode());
        System.out.println("list.eq-order " + l1.equals(new ArrayList<>(List.of("q", "p"))));
        Set<String> s1 = new HashSet<>(List.of("p", "q"));
        Set<String> s2 = new LinkedHashSet<>(List.of("q", "p"));
        System.out.println("set.hash " + (s1.hashCode() == s2.hashCode()) + " eq=" + s1.equals(s2));
        System.out.println("list-vs-set " + l1.equals(s1));

        List<Map<String, Integer>> nested1 = List.of(m());
        List<Map<String, Integer>> nested2 = List.of(m());
        System.out.println("nested.eq " + nested1.equals(nested2)
            + " contains=" + nested1.contains(m())
            + " indexOf=" + nested1.indexOf(m())
            + " hashEq=" + (nested1.hashCode() == nested2.hashCode()));
        System.out.println("objects " + Objects.equals(m(), m())
            + " " + (Objects.hashCode(m()) == m().hashCode())
            + " " + (Objects.hashCode(l1) == l1.hashCode()));

        Map.Entry<String, Integer> e1 = Map.entry("k", 7);
        Map.Entry<String, Integer> e2 = Map.entry("k", 7);
        System.out.println("entry.eq " + e1.equals(e2) + " hashEq=" + (e1.hashCode() == e2.hashCode()));
        System.out.println("entry.setValue-standalone " + m().entrySet().contains(e1));

        Map<String, Integer> keep = m();
        System.out.println("view-size " + keep.keySet().size() + " " + keep.values().size()
            + " containsAll=" + keep.keySet().containsAll(List.of("a", "c")));
    }
}
