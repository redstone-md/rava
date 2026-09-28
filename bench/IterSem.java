// Differential probe: ITERATION contracts — what an iterator sees, what it refuses, and
// what a structural modification during iteration does.
//
// WHY THIS AREA. This engine serves iteration off its own `$elems`/`$pos` backing and has a
// native iterator intrinsic that SHARES a list's array (`RAVA_ITER_INTRINSIC`), whose own
// source comment admits "a structural modify during iteration is snapshot-ish vs Java's
// CME". That is a divergence nobody has ever measured, and the shapes around it —
// `Iterator.remove`, `ListIterator.add/set`, `removeIf` during a stream, the fail-fast
// checks — are the ones a jar hits constantly and the ones where being wrong is silent.
//
// Pure JDK, no Minecraft jar, seconds to run. Every line prints an observable.
// TWO LINES ARE EXPECTED TO DIVERGE, and neither is a silent wrong answer — both are this
// engine being MORE PERMISSIVE than the JVM, which is the safe direction:
//
//   stream.anyMatchShortCircuit  the JVM threads ONE element through the pipeline at a
//                    time, so `anyMatch` stops at the first hit and `peek` has seen 1
//                    element. This engine materialises each stage before starting the next,
//                    so `peek` has seen all 3. The ANSWER is identical; only a side effect
//                    inside a stage can observe it. bench/StreamSem.java declares the same
//                    architecture for `peek.order`.
//
//   stream.iteratorReuse  a stream consumed twice is an IllegalStateException on the JVM
//                    ("stream has already been operated upon or closed"). This engine's
//                    streams are materialised collections, so a second terminal operation
//                    simply answers again. Closing it means tracking consumption on every
//                    terminal op; the failure mode of NOT doing so is a guest bug going
//                    unreported, never a wrong result.
// DIVERGES: stream.anyMatchShortCircuit
// DIVERGES: stream.iteratorReuse
import java.util.*;
import java.util.stream.*;

public class IterSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }

    static String ex(Runnable r) {
        try { r.run(); return "NO THROW"; }
        catch (Throwable t) { return t.getClass().getName(); }
    }

    static List<String> l() { return new ArrayList<>(List.of("a", "b", "c", "d")); }

    public static void main(String[] args) {
        // --- the basic contract ----------------------------------------------------
        Iterator<String> it = l().iterator();
        p("hasNext", it.hasNext());
        p("next", it.next());
        p("next2", it.next());
        p("drain", drain(l().iterator()));
        p("emptyHasNext", new ArrayList<String>().iterator().hasNext());
        p("emptyNext", ex(() -> new ArrayList<String>().iterator().next()));
        p("exhausted", exhausted());

        // remove() BEFORE next() is an IllegalStateException, and TWICE in a row too
        p("removeBeforeNext", ex(() -> l().iterator().remove()));
        p("removeTwice", removeTwice());
        p("removeWorks", removeFirst());

        // --- fail-fast: structural modification DURING iteration --------------------
        p("cme.add", cme(true));
        p("cme.remove", cme(false));
        p("cme.forEachLoop", cmeForEach());
        p("cme.iteratorRemoveOk", iteratorRemoveIsFine());
        p("cme.setIsNotStructural", setDuringIteration());
        p("cme.map", cmeMap());
        p("cme.keySet", cmeKeySet());

        // --- ListIterator -----------------------------------------------------------
        p("listIter.indexes", listIterIndexes());
        p("listIter.backwards", listIterBackwards());
        p("listIter.set", listIterSet());
        p("listIter.add", listIterAdd());
        p("listIter.fromIndex", listIterFrom());
        p("listIter.previousEmpty", ex(() -> l().listIterator().previous()));

        // --- removeIf / bulk mutators and their RETURN values ------------------------
        List<String> r1 = l();
        p("removeIf.true", r1.removeIf(s -> s.equals("b")));
        p("removeIf.result", r1);
        p("removeIf.false", l().removeIf(s -> s.equals("zz")));
        List<String> r2 = l();
        p("addAll.changed", r2.addAll(List.of("e")));
        p("addAll.empty", r2.addAll(List.of()));
        p("removeAll.changed", r2.removeAll(List.of("a", "zz")));
        p("retainAll.changed", r2.retainAll(List.of("b", "c")));
        p("retainAll.same", r2.retainAll(List.of("b", "c", "d")));
        p("r2.final", r2);
        p("replaceAll", replaceAll());
        p("list.remove.byIndex", l().remove(1));
        p("list.remove.byObject", l().remove("b"));
        p("list.remove.missing", l().remove("zz"));

        // --- iteration order across the collection kinds ------------------------------
        p("order.arrayList", String.join("", l()));
        p("order.linkedHashSet", new LinkedHashSet<>(List.of("c", "a", "b")).toString());
        p("order.treeSet", new TreeSet<>(List.of("c", "a", "b")).toString());
        p("order.arrayDeque", new ArrayDeque<>(List.of("a", "b")).toString());
        p("order.linkedList", new LinkedList<>(List.of("a", "b")).toString());
        p("order.descendingSet", new TreeSet<>(List.of("a", "b", "c")).descendingSet().toString());
        p("order.reversedList", reversedList());

        // --- streams over a collection being iterated ---------------------------------
        p("stream.count", l().stream().count());
        p("stream.limitLazy", Stream.iterate(1, i -> i * 2).limit(5).collect(Collectors.toList()));
        p("stream.findFirst", l().stream().filter(s -> s.compareTo("b") > 0).findFirst().orElse("?"));
        p("stream.anyMatchShortCircuit", anyMatchCounts());
        p("stream.iteratorReuse", ex(() -> { var s = l().stream(); s.count(); s.count(); }));
        p("spliterator.size", l().spliterator().estimateSize());

        // --- Iterable.forEach vs the enhanced for ------------------------------------
        StringBuilder b = new StringBuilder();
        l().forEach(b::append);
        p("forEach", b);
        p("forEachRemaining", forEachRemaining());
    }

    static String drain(Iterator<String> it) {
        StringBuilder b = new StringBuilder();
        while (it.hasNext()) { b.append(it.next()); }
        return b.toString();
    }

    static String exhausted() {
        Iterator<String> it = l().iterator();
        while (it.hasNext()) { it.next(); }
        return ex(it::next);
    }

    static String removeTwice() {
        Iterator<String> it = l().iterator();
        it.next();
        it.remove();
        return ex(it::remove);
    }

    static String removeFirst() {
        List<String> l = l();
        Iterator<String> it = l.iterator();
        it.next();
        it.remove();
        return l.toString();
    }

    static String cme(boolean add) {
        List<String> l = l();
        return ex(() -> {
            for (String s : l) {
                if (s.equals("b")) {
                    if (add) { l.add("z"); } else { l.remove("d"); }
                }
            }
        });
    }

    static String cmeForEach() {
        List<String> l = l();
        return ex(() -> l.forEach(s -> { if (s.equals("b")) { l.add("z"); } }));
    }

    static String iteratorRemoveIsFine() {
        List<String> l = l();
        Iterator<String> it = l.iterator();
        while (it.hasNext()) { if (it.next().equals("b")) { it.remove(); } }
        return l.toString();
    }

    static String setDuringIteration() {
        List<String> l = l();
        return ex(() -> {
            for (int i = 0; i < l.size(); i++) { l.set(i, l.get(i).toUpperCase()); }
            for (String s : l) { l.set(0, s); }
        });
    }

    static String cmeMap() {
        Map<String, Integer> m = new LinkedHashMap<>(Map.of("a", 1));
        return ex(() -> { for (var e : m.entrySet()) { m.put("b", 2); } });
    }

    static String cmeKeySet() {
        Map<String, Integer> m = new LinkedHashMap<>(Map.of("a", 1));
        return ex(() -> { for (String k : m.keySet()) { m.remove("a"); m.put("c", 3); } });
    }

    static String listIterIndexes() {
        ListIterator<String> li = l().listIterator();
        StringBuilder b = new StringBuilder();
        while (li.hasNext()) { b.append(li.nextIndex()).append(li.next()); }
        return b.toString();
    }

    static String listIterBackwards() {
        List<String> l = l();
        ListIterator<String> li = l.listIterator(l.size());
        StringBuilder b = new StringBuilder();
        while (li.hasPrevious()) { b.append(li.previousIndex()).append(li.previous()); }
        return b.toString();
    }

    static String listIterSet() {
        List<String> l = l();
        ListIterator<String> li = l.listIterator();
        li.next();
        li.set("A");
        return l.toString();
    }

    static String listIterAdd() {
        List<String> l = l();
        ListIterator<String> li = l.listIterator();
        li.next();
        li.add("X");
        return l + "|" + li.next();
    }

    static String listIterFrom() {
        ListIterator<String> li = l().listIterator(2);
        return li.next() + li.previous();
    }

    static String replaceAll() {
        List<String> l = l();
        l.replaceAll(String::toUpperCase);
        return l.toString();
    }

    static String reversedList() {
        List<String> l = l();
        Collections.reverse(l);
        return l.toString();
    }

    static String anyMatchCounts() {
        int[] seen = {0};
        boolean r = Stream.of("a", "b", "c").peek(s -> seen[0]++).anyMatch(s -> s.equals("a"));
        return r + ":" + seen[0];
    }

    static String forEachRemaining() {
        Iterator<String> it = l().iterator();
        it.next();
        StringBuilder b = new StringBuilder();
        it.forEachRemaining(b::append);
        return b.toString();
    }
}
