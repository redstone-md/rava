// Differential probe: equality, hashing and ordering across the JDK types this engine
// models natively.
//
// WHY THIS AREA. rava does not use the JDK's own collections — it models maps, sets and
// lists with synthetic fields and derives map keys through its own `structural_key` /
// `content_key` machinery, which has been rewritten for speed several times (key interning,
// the `$skey` memo, `equals_kind`, the sort key cache). Every one of those is a chance for
// two objects that the JVM calls equal to stop colliding, or for two that differ to start.
// A wrong answer here is SILENT: a map simply behaves as if a key were never inserted.
//
// It also exercises the write-through views fixed on 2026-08-16 (subList / Arrays.asList),
// since a view's equals and hashCode must answer against the BACKING data, not a snapshot.
//
// Every line prints an observable. Pure JDK, no Minecraft jar, seconds to run.
// EVERY LINE MATCHES `java -Xint`; there are no declared divergences.
//
// ⭐**`shortVsInt` AND `byteVsInt` WERE DECLARED HERE AND ARE NOW CLOSED.** The declaration
// read: "this engine has no variant for the NARROW boxes — Short, Byte, Character and Integer
// all arrive as `Value::Int` — so the typed check that separates Integer from Long cannot
// separate these four from each other... closing it means giving boxed values a type, which is
// a change to the value model."
//
// That change is `crate::boxid`: `Short.valueOf`, `Byte.valueOf` and `Character.valueOf` mint
// REAL heap objects of their own class, so `Short.valueOf((short)1).equals(Integer.valueOf(1))`
// is false here as it is on the JVM. Both lines are COMPARED now, deliberately — they are the
// regression test for that work, and re-declaring either would hide it. `RAVA_NO_BOXID=1`
// restores the old representation and both lines diverge again.
import java.util.*;

public class EqualsSem {
    record Pt(int x, int y) {}
    record Nested(Pt a, List<String> b) {}
    static class Plain { int v; Plain(int v) { this.v = v; } }
    static class ValueLike {
        final int v;
        ValueLike(int v) { this.v = v; }
        @Override public boolean equals(Object o) { return o instanceof ValueLike w && w.v == v; }
        @Override public int hashCode() { return v; }
        @Override public String toString() { return "VL(" + v + ")"; }
    }

    static void p(String k, Object v) { System.out.println(k + " " + v); }

    public static void main(String[] a) {
        // --- records: equality, hash agreement, and use AS KEYS -------------------
        p("rec.equals", new Pt(1, 2).equals(new Pt(1, 2)));
        p("rec.hashEq", new Pt(1, 2).hashCode() == new Pt(1, 2).hashCode());
        p("rec.notEq", new Pt(1, 2).equals(new Pt(2, 1)));
        Map<Pt, String> rm = new HashMap<>();
        rm.put(new Pt(3, 4), "v");
        p("rec.key", rm.get(new Pt(3, 4)));
        p("rec.keyMiss", rm.get(new Pt(4, 3)));
        p("rec.toString", new Pt(1, 2).toString());
        // a record CONTAINING a record and a list — the nested key derivation
        Map<Nested, String> nm = new HashMap<>();
        nm.put(new Nested(new Pt(1, 1), List.of("a", "b")), "deep");
        p("rec.nestedKey", nm.get(new Nested(new Pt(1, 1), List.of("a", "b"))));
        p("rec.nestedMiss", nm.get(new Nested(new Pt(1, 1), List.of("b", "a"))));

        // --- identity vs value equality ------------------------------------------
        Plain p1 = new Plain(1), p2 = new Plain(1);
        p("plain.equals", p1.equals(p2));
        p("plain.self", p1.equals(p1));
        Map<Plain, String> pm = new HashMap<>();
        pm.put(p1, "x");
        p("plain.keySelf", pm.get(p1));
        p("plain.keyOther", pm.get(p2));
        p("value.equals", new ValueLike(7).equals(new ValueLike(7)));
        Map<ValueLike, String> vm = new HashMap<>();
        vm.put(new ValueLike(7), "y");
        p("value.key", vm.get(new ValueLike(7)));

        // --- collection equality: kind matters -----------------------------------
        p("list.eq", List.of(1, 2, 3).equals(Arrays.asList(1, 2, 3)));
        p("list.order", List.of(1, 2, 3).equals(List.of(3, 2, 1)));
        p("set.eq", Set.of(1, 2, 3).equals(new HashSet<>(List.of(3, 2, 1))));
        p("listVsSet", List.of(1, 2, 3).equals(Set.of(1, 2, 3)));
        p("map.eq", Map.of("a", 1).equals(new HashMap<>(Map.of("a", 1))));
        p("list.hashEq", List.of(1, 2, 3).hashCode() == Arrays.asList(1, 2, 3).hashCode());
        p("set.hashEq", Set.of(1, 2).hashCode() == new HashSet<>(List.of(2, 1)).hashCode());
        p("map.hashEq", Map.of("a", 1).hashCode() == new HashMap<>(Map.of("a", 1)).hashCode());
        p("emptyList.eq", List.of().equals(new ArrayList<>()));
        p("emptyMap.eq", Map.of().equals(new HashMap<>()));

        // --- the JDK's specified hash FORMULAS ------------------------------------
        p("list.hash", List.of("a", "b").hashCode());
        p("set.hash", Set.of("a").hashCode());
        p("map.hash", Map.of("a", 1).hashCode());
        p("arrays.hash", Arrays.hashCode(new int[] {1, 2, 3}));
        p("arrays.deepHash", Arrays.deepHashCode(new Object[] {new int[] {1}, "s"}));
        p("objects.hash", Objects.hash("a", 1, true));
        p("string.hash", "minecraft:stone".hashCode());
        p("int.hash", Integer.valueOf(42).hashCode());
        p("long.hash", Long.valueOf(1L << 40).hashCode());
        p("double.hash", Double.valueOf(1.5).hashCode());
        p("bool.hash", Boolean.TRUE.hashCode());
        p("char.hash", Character.valueOf('z').hashCode());
        p("null.hash", Objects.hashCode(null));

        // --- VIEWS must answer against the backing data ---------------------------
        List<Integer> base = new ArrayList<>(List.of(0, 1, 2, 3, 4, 5));
        List<Integer> sub = base.subList(1, 4);
        p("sub.eq", sub.equals(List.of(1, 2, 3)));
        p("sub.hashEq", sub.hashCode() == List.of(1, 2, 3).hashCode());
        base.set(1, 99);
        p("sub.afterWrite", sub.equals(List.of(99, 2, 3)));
        Integer[] arr = {7, 8};
        List<Integer> fixed = Arrays.asList(arr);
        p("asList.eq", fixed.equals(List.of(7, 8)));
        arr[0] = 9;
        p("asList.afterWrite", fixed.equals(List.of(9, 8)));

        // --- ordering -------------------------------------------------------------
        p("cmp.int", Integer.compare(3, 7));
        p("cmp.str", "abc".compareTo("abd"));
        p("cmp.strCase", String.CASE_INSENSITIVE_ORDER.compare("ABC", "abc"));
        List<String> s = new ArrayList<>(List.of("b", "A", "c", "B", "a"));
        s.sort(String.CASE_INSENSITIVE_ORDER);
        p("sort.caseInsensitive", s);
        List<String> t = new ArrayList<>(List.of("bb", "a", "ccc", "dd"));
        t.sort(Comparator.comparingInt(String::length).thenComparing(Comparator.naturalOrder()));
        p("sort.composite", t);
        p("treeMap.order", new TreeMap<>(Map.of("c", 1, "a", 2, "b", 3)).toString());
        p("treeSet.order", new TreeSet<>(List.of(5, 1, 3)).toString());
        p("sortNulls", sortWithNulls());
        // sorting STABILITY — equal keys keep their input order
        List<String> st = new ArrayList<>(List.of("bb", "aa", "cc"));
        st.sort(Comparator.comparingInt(String::length));
        p("sort.stable", st);

        // --- equals CONTRACT corners ----------------------------------------------
        p("eq.null", "a".equals(null));
        p("eq.otherType", "1".equals(1));
        p("objects.equalsNulls", Objects.equals(null, null));
        p("objects.equalsOne", Objects.equals(null, "a"));
        p("intVsLong", Integer.valueOf(1).equals(Long.valueOf(1L)));
        p("doubleNaN", Double.valueOf(Double.NaN).equals(Double.valueOf(Double.NaN)));
        p("doubleNaNeq", Double.NaN == Double.NaN);
        p("doubleZero", Double.valueOf(0.0).equals(Double.valueOf(-0.0)));
        p("doubleZeroEq", 0.0 == -0.0);
        // CROSS-TYPE equality, which is where a content-keyed engine is most exposed:
        // `Integer.valueOf(1)` and `Long.valueOf(1L)` render identically and must still
        // answer false, while same-type pairs must keep answering true.
        p("longVsLong", Long.valueOf(1L).equals(Long.valueOf(1L)));
        p("intVsInt", Integer.valueOf(1).equals(Integer.valueOf(1)));
        p("longFromInt", Long.valueOf(1).equals(Long.valueOf(1L)));
        p("doubleVsInt", Double.valueOf(1).equals(Integer.valueOf(1)));
        p("doubleVsDouble", Double.valueOf(1.0).equals(Double.valueOf(1.0)));
        p("shortVsInt", Short.valueOf((short) 1).equals(Integer.valueOf(1)));
        p("byteVsInt", Byte.valueOf((byte) 1).equals(Integer.valueOf(1)));
        p("floatVsDouble", Float.valueOf(1f).equals(Double.valueOf(1.0)));
        // CONTROL PAIRS: the same-type answers a tightened check must not break.
        p("boolVsBool", Boolean.TRUE.equals(Boolean.TRUE));
        p("boolFalse", Boolean.FALSE.equals(Boolean.valueOf(false)));
        p("boolVsBoolF", Boolean.TRUE.equals(Boolean.FALSE));
        p("boolBoxed", Boolean.valueOf("true").equals(Boolean.TRUE));
        p("strVsStr", "abc".equals(new String("abc".toCharArray())));
        p("strEmpty", "".equals(""));
        p("charVsChar", Character.valueOf('a').equals(Character.valueOf('a')));
        p("floatVsFloat", Float.valueOf(1f).equals(Float.valueOf(1f)));
        p("strVsSb", "a".equals(new StringBuilder("a")));
        p("strVsChar", "a".equals('a'));
        p("sbVsStr", new StringBuilder("a").equals("a"));
        p("boolVsInt", Boolean.TRUE.equals(Integer.valueOf(1)));
        p("listVsArray", List.of(1).equals(new int[] {1}));
        // and the same question asked through a MAP, which is how it actually bites
        Map<Long, String> lm = new HashMap<>();
        lm.put(1L, "long");
        p("map.longKeyByInt", lm.get(Integer.valueOf(1)));
        p("map.longKeyByLong", lm.get(Long.valueOf(1L)));
        Map<String, String> sm2 = new HashMap<>();
        sm2.put("1", "str");
        p("map.strKeyByInt", sm2.get(Integer.valueOf(1)));
        p("map.strKeyByStr", sm2.get("1"));

        Map<Double, String> dm = new HashMap<>();
        dm.put(Double.NaN, "nan");
        p("nanAsKey", dm.get(Double.NaN));
    }

    static String sortWithNulls() {
        try {
            List<String> l = new ArrayList<>(Arrays.asList("b", null, "a"));
            l.sort(Comparator.nullsFirst(Comparator.naturalOrder()));
            return l.toString();
        } catch (Throwable t) {
            return t.getClass().getName();
        }
    }
}
