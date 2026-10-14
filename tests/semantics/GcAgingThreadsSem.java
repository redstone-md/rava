import java.util.*;
import java.util.concurrent.*;

// GcAgingSem on eight threads at once, each with its own deterministic graph, plus a shared
// concurrent map they all write: collections stop threads in the middle of linking young
// objects into old ones.
public class GcAgingThreadsSem {
    static final class Node {
        Node next;
        Object payload;
        long value;
        Node(long v) { value = v; }
    }

    static final ConcurrentHashMap<Integer, Node> shared = new ConcurrentHashMap<>();

    static long work(int seed) {
        Random r = new Random(seed);
        Node[] old = new Node[1000];
        for (int i = 0; i < old.length; i++) old[i] = new Node(i);
        long total = 0;
        for (int round = 0; round < 40; round++) {
            for (int i = 0; i < 60_000; i++) new Node(i).payload = new int[8];
            for (int i = 0; i < 2000; i++) {
                Node n = new Node(round * 10_000L + i + seed);
                n.next = old[r.nextInt(old.length)].next;
                old[r.nextInt(old.length)].next = n;
                old[r.nextInt(old.length)].payload = n;
                if ((i & 31) == 0) shared.put(seed * 1000 + r.nextInt(1000), n);
            }
            for (int i = 0; i < 1500; i++) {
                Node o = old[r.nextInt(old.length)];
                if (o.next != null && r.nextBoolean()) o.next = o.next.next;
            }
            long s = 0;
            for (Node o : old) {
                Node n = o;
                for (int k = 0; n != null && k < 30; k++, n = n.next) s = s * 31 + n.value;
                if (o.payload instanceof Node p) s += p.value;
            }
            total = total * 13 + s;
        }
        return total;
    }

    public static void main(String[] args) throws Exception {
        ExecutorService pool = Executors.newFixedThreadPool(8);
        List<Future<Long>> results = new ArrayList<>();
        for (int t = 0; t < 8; t++) {
            int seed = t;
            results.add(pool.submit(() -> work(seed)));
        }
        for (int t = 0; t < 8; t++) System.out.println("thread " + t + " " + results.get(t).get());
        pool.shutdown();
        long s = 0;
        for (Map.Entry<Integer, Node> e : new TreeMap<>(shared).entrySet()) s += e.getKey() ^ (e.getValue().value & 0xffff);
        System.out.println("shared " + shared.size() + " " + (s != 0));
    }
}
