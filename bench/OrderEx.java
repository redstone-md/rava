// ORDER contracts: sort stability, comparator semantics, Deque/Queue ends,
// ListIterator, and the navigable-set/map queries.
//
// Stability is the one with teeth for this engine: MC's registries, tag
// resolution and datapack merges sort by a key that ties constantly, and an
// unstable sort makes the RESULT depend on the sort implementation — a
// determinism bug that no gate hash over worldgen arithmetic can see.
import java.util.*;

public class OrderEx {
    record Item(String key, int ord) {
        @Override public String toString() { return key + ord; }
    }

    static void p(String tag, java.util.function.Supplier<Object> s) {
        try { System.out.println(tag + " " + s.get()); }
        catch (Throwable t) { System.out.println(tag + " !" + t.getClass().getName() + ":" + t.getMessage()); }
    }

    static List<Item> ties() {
        return new ArrayList<>(List.of(
            new Item("a", 1), new Item("b", 1), new Item("c", 1),
            new Item("d", 0), new Item("e", 0), new Item("f", 1)));
    }

    public static void main(String[] args) {
        // --- stability ---------------------------------------------------------
        p("sort-stable", () -> { List<Item> l = ties(); l.sort(Comparator.comparingInt(Item::ord)); return l; });
        p("sort-stable-str", () -> {
            List<String> l = new ArrayList<>(List.of("bb", "aa", "cc", "dd"));
            l.sort(Comparator.comparingInt(String::length));
            return l;
        });
        p("collections-sort-stable", () -> { List<Item> l = ties(); Collections.sort(l, Comparator.comparingInt(Item::ord)); return l; });
        p("stream-sorted-stable", () -> ties().stream().sorted(Comparator.comparingInt(Item::ord)).toList());

        // --- comparator composition --------------------------------------------
        p("cmp-then", () -> {
            List<Item> l = ties();
            l.sort(Comparator.comparingInt(Item::ord).thenComparing(Item::key));
            return l;
        });
        p("cmp-reversed", () -> {
            List<Item> l = ties();
            l.sort(Comparator.comparingInt(Item::ord).reversed());
            return l;
        });
        p("cmp-natural-reverse", () -> {
            List<String> l = new ArrayList<>(List.of("b", "a", "c"));
            l.sort(Comparator.reverseOrder());
            return l;
        });
        p("cmp-nullsfirst", () -> {
            List<String> l = new ArrayList<>(Arrays.asList("b", null, "a"));
            l.sort(Comparator.nullsFirst(Comparator.naturalOrder()));
            return l;
        });
        p("cmp-inconsistent", () -> {
            List<Integer> l = new ArrayList<>(List.of(3, 1, 2));
            l.sort((x, y) -> 0);
            return l;
        });

        // --- Deque / Queue ends -------------------------------------------------
        p("deque", () -> {
            Deque<String> d = new ArrayDeque<>();
            d.addFirst("b"); d.addLast("c"); d.push("a"); d.offer("d");
            return d + " peek=" + d.peek() + " peekLast=" + d.peekLast()
                + " pop=" + d.pop() + " pollLast=" + d.pollLast() + " left=" + d;
        });
        p("deque-empty", () -> new ArrayDeque<String>().removeFirst());
        p("deque-poll-empty", () -> new ArrayDeque<String>().pollFirst());
        p("queue", () -> {
            Queue<String> q = new LinkedList<>(List.of("a", "b"));
            return q.poll() + q.peek() + q.size();
        });
        p("queue-element-empty", () -> new LinkedList<String>().element());
        p("pq", () -> {
            PriorityQueue<Integer> q = new PriorityQueue<>(List.of(5, 1, 3));
            return q.poll() + "" + q.poll() + q.poll() + " " + q.poll();
        });
        p("pq-cmp", () -> {
            PriorityQueue<Integer> q = new PriorityQueue<>(Comparator.reverseOrder());
            q.addAll(List.of(5, 1, 3));
            return q.poll() + "" + q.poll();
        });
        p("stack", () -> {
            Stack<String> s = new Stack<>();
            s.push("a"); s.push("b");
            return s + " " + s.pop() + s.peek() + s.search("a");
        });

        // --- ListIterator --------------------------------------------------------
        p("listiter", () -> {
            List<String> l = new ArrayList<>(List.of("a", "b", "c"));
            ListIterator<String> it = l.listIterator();
            StringBuilder sb = new StringBuilder();
            while (it.hasNext()) { sb.append(it.nextIndex()).append(it.next()); }
            while (it.hasPrevious()) { sb.append(it.previousIndex()).append(it.previous()); }
            return sb.toString();
        });
        p("listiter-set", () -> {
            List<String> l = new ArrayList<>(List.of("a", "b"));
            ListIterator<String> it = l.listIterator();
            it.next(); it.set("z");
            return l.toString();
        });
        p("listiter-add", () -> {
            List<String> l = new ArrayList<>(List.of("a", "c"));
            ListIterator<String> it = l.listIterator();
            it.next(); it.add("b");
            return l + " next=" + it.next();
        });
        p("iter-remove-twice", () -> {
            List<String> l = new ArrayList<>(List.of("a", "b"));
            Iterator<String> it = l.iterator();
            it.next(); it.remove(); it.remove();
            return l.toString();
        });
        p("iter-remove-before-next", () -> { new ArrayList<>(List.of("a")).iterator().remove(); return "no-throw"; });

        // --- navigable ------------------------------------------------------------
        p("treeset", () -> {
            TreeSet<Integer> t = new TreeSet<>(List.of(5, 1, 3));
            return t + " first=" + t.first() + " last=" + t.last()
                + " ceil=" + t.ceiling(2) + " floor=" + t.floor(2)
                + " higher=" + t.higher(3) + " lower=" + t.lower(3)
                + " head=" + t.headSet(3) + " tail=" + t.tailSet(3)
                + " desc=" + t.descendingSet();
        });
        p("treemap", () -> {
            TreeMap<String, Integer> m = new TreeMap<>();
            m.put("pear", 3); m.put("apple", 1); m.put("fig", 2);
            return m + " first=" + m.firstKey() + " last=" + m.lastKey()
                + " ceil=" + m.ceilingKey("b") + " head=" + m.headMap("fig")
                + " keys=" + m.keySet() + " values=" + m.values();
        });
        p("treeset-cmp", () -> {
            TreeSet<Integer> t = new TreeSet<>(Comparator.reverseOrder());
            t.addAll(List.of(1, 5, 3));
            return t.toString();
        });
        p("linked", () -> {
            LinkedHashMap<String, Integer> m = new LinkedHashMap<>();
            m.put("z", 1); m.put("a", 2); m.put("z", 3);
            return m + " " + m.keySet();
        });

        // --- binarySearch / minmax --------------------------------------------------
        p("minmax", () -> Collections.min(List.of(3, 1, 2)) + " " + Collections.max(List.of(3, 1, 2))
            + " " + Collections.min(List.of("b", "a"), Comparator.reverseOrder()));
        p("minmax-empty", () -> Collections.min(new ArrayList<Integer>()));
        p("binsearch-list", () -> Collections.binarySearch(List.of(1, 3, 5), 3)
            + " " + Collections.binarySearch(List.of(1, 3, 5), 4));
        p("frequency-swap", () -> {
            List<String> l = new ArrayList<>(List.of("a", "b", "a"));
            Collections.swap(l, 0, 1);
            return l + " freq=" + Collections.frequency(l, "a");
        });
    }
}
