// Differential probe: collection semantics the tree has never checked against java -Xint.
// Pure JDK, no Minecraft jar, so it runs in seconds. Every line prints an observable.
// SIX LINES DIVERGE ACROSS THIS PROBE AND bench\ExcSem.java AND NONE OF THEM IS
// DECLARED, because none of them is an accepted limit. They are defects, left RED on
// purpose so bench\sem_all.ps1 keeps reporting them:
//
//   sublist.write-through   `base.subList(1,4).set(0,99)` must write THROUGH to `base`.
//   sublist.clear-through   `sub.clear()` must remove that range FROM `base`.
//   asList.write-through    `Arrays.asList(arr).set(0,9)` must store into `arr`.
//
// All three are the same bug: these are specified as VIEWS over the backing storage and
// this engine returns COPIES. A copy answers every read correctly, which is why nothing
// caught it - the divergence is only observable through the original, and then it is a
// silently wrong answer rather than an error. The jar uses both constructions.
import java.util.*;
public class CollSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    public static void main(String[] a) {
        // --- ArrayDeque: both ends, and the null ban
        Deque<Integer> dq = new ArrayDeque<>();
        dq.addFirst(1); dq.addLast(2); dq.push(0); dq.offerLast(3);
        p("deque", dq);
        p("deque.peek", dq.peek() + " " + dq.peekFirst() + " " + dq.peekLast());
        p("deque.pop", dq.pop() + " then " + dq);
        p("deque.pollLast", dq.pollLast() + " then " + dq);
        try { dq.addFirst(null); p("deque.null", "NO THROW"); }
        catch (Exception e) { p("deque.null", e.getClass().getName()); }
        Deque<Integer> dq2 = new ArrayDeque<>(List.of(1, 2, 3));
        p("deque.descending", iter(dq2.descendingIterator()));
        p("deque.removeFirstOccurrence", dq2.removeFirstOccurrence(2) + " " + dq2);

        // --- PriorityQueue: heap order on poll, NOT on toString/iteration
        PriorityQueue<Integer> pq = new PriorityQueue<>(List.of(5, 1, 4, 2, 3));
        StringBuilder drained = new StringBuilder();
        PriorityQueue<Integer> copy = new PriorityQueue<>(pq);
        while (!copy.isEmpty()) drained.append(copy.poll()).append(' ');
        p("pq.drain", drained.toString().trim());
        p("pq.peek", pq.peek());
        p("pq.size", pq.size());
        PriorityQueue<String> pqc = new PriorityQueue<>(Comparator.reverseOrder());
        pqc.addAll(List.of("a", "c", "b"));
        p("pq.cmp.poll", pqc.poll() + " " + pqc.poll() + " " + pqc.poll());

        // --- ArrayList.clear: preserve normal result and fail-fast iterators
        List<Integer> cleared = new ArrayList<>(List.of(1, 2, 3));
        Iterator<Integer> stale = cleared.iterator();
        cleared.clear();
        String staleNext;
        try { stale.next(); staleNext = "NO THROW"; }
        catch (Exception e) { staleNext = e.getClass().getName(); }
        p("arraylist.clear", cleared + " " + staleNext);

        // --- subList: a VIEW, and its writes go through
        List<Integer> base = new ArrayList<>(List.of(0, 1, 2, 3, 4, 5));
        List<Integer> sub = base.subList(1, 4);
        p("sublist", sub);
        sub.set(0, 99);
        p("sublist.write-through", base);
        sub.clear();
        p("sublist.clear-through", base);

        // --- Arrays.asList: fixed size, write-through to the array
        Integer[] arr = { 1, 2, 3 };
        List<Integer> fixed = Arrays.asList(arr);
        fixed.set(0, 9);
        p("asList.write-through", arr[0] + " " + fixed);
        try { fixed.add(4); p("asList.add", "NO THROW"); }
        catch (Exception e) { p("asList.add", e.getClass().getName()); }
        try { fixed.remove(0); p("asList.remove", "NO THROW"); }
        catch (Exception e) { p("asList.remove", e.getClass().getName()); }

        // --- Comparator chains
        List<String> words = new ArrayList<>(List.of("bb", "a", "ccc", "dd", "e"));
        words.sort(Comparator.comparingInt(String::length).thenComparing(Comparator.naturalOrder()));
        p("cmp.len-then-nat", words);
        words.sort(Comparator.comparingInt(String::length).reversed());
        p("cmp.reversed-stable", words);
        List<String> withNull = new ArrayList<>(Arrays.asList("b", null, "a"));
        withNull.sort(Comparator.nullsFirst(Comparator.naturalOrder()));
        p("cmp.nullsFirst", withNull);

        // --- Map.compute family and the null contract
        Map<String, Integer> m = new HashMap<>();
        m.put("x", 1);
        p("computeIfAbsent", m.computeIfAbsent("y", k -> 2) + " " + m.computeIfAbsent("x", k -> 99));
        p("computeIfPresent-null-removes", m.computeIfPresent("x", (k, v) -> null) + " " + m);
        p("merge", m.merge("y", 10, Integer::sum) + " " + m.merge("z", 7, Integer::sum));
        p("merge-null-removes", m.merge("y", 1, (v1, v2) -> null) + " " + m);
        p("getOrDefault", m.getOrDefault("nope", -1));
        p("putIfAbsent", m.putIfAbsent("z", 100) + " " + m.get("z"));
        p("replaceAll", replaceAll(m));

        // --- LinkedHashMap access order
        LinkedHashMap<String, Integer> lru = new LinkedHashMap<>(16, 0.75f, true);
        lru.put("a", 1); lru.put("b", 2); lru.put("c", 3);
        lru.get("a");
        p("lru.access-order", lru.keySet());

        // --- List.of / Set.of / Map.of rejections
        try { List.of(1, null); p("List.of.null", "NO THROW"); }
        catch (Exception e) { p("List.of.null", e.getClass().getName()); }
        try { Set.of(1, 1); p("Set.of.dup", "NO THROW"); }
        catch (Exception e) { p("Set.of.dup", e.getClass().getName() + ": " + e.getMessage()); }
        try { Map.of("k", 1, "k", 2); p("Map.of.dupkey", "NO THROW"); }
        catch (Exception e) { p("Map.of.dupkey", e.getClass().getName() + ": " + e.getMessage()); }

        // --- equals/hashCode across implementations
        p("list.equals", new ArrayList<>(List.of(1, 2)).equals(new LinkedList<>(List.of(1, 2))));
        p("set.equals", new HashSet<>(List.of(1, 2)).equals(new TreeSet<>(List.of(2, 1))));
        p("map.equals", new HashMap<>(Map.of("a", 1)).equals(new TreeMap<>(Map.of("a", 1))));
        p("list.hash", new ArrayList<>(List.of(1, 2)).hashCode() == new LinkedList<>(List.of(1, 2)).hashCode());
        p("set.hash", new HashSet<>(List.of(1, 2)).hashCode() == new TreeSet<>(List.of(2, 1)).hashCode());
    }
    static String iter(Iterator<?> it) { StringBuilder b = new StringBuilder(); while (it.hasNext()) b.append(it.next()).append(' '); return b.toString().trim(); }
    static String replaceAll(Map<String, Integer> m) { Map<String, Integer> c = new TreeMap<>(m); c.replaceAll((k, v) -> v * 2); return c.toString(); }
}
