// Collection ELEMENT identity: what counts as "the same element".
//
// The engine used to key elements by their RENDERED TEXT, so Integer 1 and "1"
// were one element, and any object without a synthetic text field was keyed by
// handle, so two equal records were two elements. Both are silent wrong answers
// a gate hash cannot see. Diff against `java -Xint`.
import java.util.*;

public class ElemEq {
    record P(int x, int y) {}
    static class Ident { final int v; Ident(int v) { this.v = v; } public String toString() { return "I" + v; } }

    public static void main(String[] a) {
        List<Object> mixed = new ArrayList<>(List.of(1, "1", 1L, 1.0));
        System.out.println("cross-type " + mixed.contains(1) + " " + mixed.contains("1")
            + " " + mixed.indexOf("1") + " " + mixed.indexOf(1) + " " + mixed.indexOf(1L));
        System.out.println("null-vs-text " + new ArrayList<>(Arrays.asList("null")).contains(null)
            + " " + new ArrayList<>(Arrays.asList("null")).indexOf(null));

        Set<Object> s = new HashSet<>();
        s.add(1); s.add("1"); s.add(1L);
        System.out.println("set-cross size=" + s.size());

        Set<P> ps = new HashSet<>();
        ps.add(new P(1, 2)); ps.add(new P(1, 2)); ps.add(new P(3, 4));
        System.out.println("record-set size=" + ps.size() + " contains=" + ps.contains(new P(1, 2)));

        List<P> pl = new ArrayList<>(List.of(new P(1, 2), new P(3, 4)));
        System.out.println("record-list contains=" + pl.contains(new P(3, 4))
            + " indexOf=" + pl.indexOf(new P(3, 4))
            + " remove=" + pl.remove(new P(1, 2)) + " left=" + pl);

        Set<Ident> is = new HashSet<>();
        Ident i1 = new Ident(5), i2 = new Ident(5);
        is.add(i1); is.add(i2);
        System.out.println("identity-set size=" + is.size() + " contains-other=" + is.contains(new Ident(5))
            + " contains-same=" + is.contains(i1));

        System.out.println("copy-ctor " + new HashSet<>(List.of("x", "y")).size()
            + " " + new HashSet<>(Arrays.asList("x", "x", "y")).size()
            + " " + new TreeSet<>(Arrays.asList("b", "a", "b")).size()
            + " " + new TreeSet<>(Arrays.asList("b", "a", "b")));

        List<Object> l = new ArrayList<>(List.of("a", "b", "a"));
        System.out.println("last " + l.lastIndexOf("a") + " " + l.indexOf("a"));
        System.out.println("bulk " + l.containsAll(List.of("a", "b"))
            + " " + l.containsAll(List.of("a", "z")));
        List<Object> r = new ArrayList<>(List.of(1, "1", 2));
        r.removeAll(List.of("1"));
        System.out.println("removeAll-cross " + r);
        List<Object> t = new ArrayList<>(List.of(1, "1", 2));
        t.retainAll(List.of(1, 2));
        System.out.println("retainAll-cross " + t);

        List<Object> f = new ArrayList<>(List.of(-0.0, 0.0));
        System.out.println("float-zero " + f.indexOf(0.0) + " " + f.indexOf(-0.0));

        Set<String> ss = new LinkedHashSet<>();
        ss.add("k"); ss.add("k");
        System.out.println("string-set " + ss + " " + ss.size());

        Map<P, String> pm = new HashMap<>();
        pm.put(new P(1, 1), "v");
        System.out.println("record-key " + pm.get(new P(1, 1)) + " " + pm.containsKey(new P(1, 1)));
    }
}
