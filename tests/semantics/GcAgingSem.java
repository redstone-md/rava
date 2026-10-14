import java.util.*;

// Objects that outlive a young collection or two and then die, while old objects, arrays,
// maps and statics keep pointing at the newest ones: what a collector that promotes survivors
// (and gives them a second chance) must keep exactly right. Deterministic; every round's graph
// is checked against a checksum of what was stored in it.
public class GcAgingSem {
    static final class Node {
        Node next;
        Object[] slots = new Object[3];
        long value;
        Node(long v) { value = v; }
    }

    static Node recentStatic;
    static Object[] staticArray = new Object[64];
    static final Map<Integer, Node> map = new HashMap<>();

    static long sum(Node n, int limit) {
        long s = 0;
        for (int i = 0; n != null && i < limit; i++, n = n.next) {
            s = s * 31 + n.value;
            for (Object o : n.slots) if (o instanceof Node m) s += m.value;
        }
        return s;
    }

    public static void main(String[] args) {
        Random r = new Random(42);
        Node[] old = new Node[2000];
        for (int i = 0; i < old.length; i++) old[i] = new Node(i);
        long total = 0;
        for (int round = 0; round < 60; round++) {
            // Young garbage, to trigger young collections.
            for (int i = 0; i < 320_000; i++) {
                Node g = new Node(i);
                g.slots[0] = new byte[64];
            }
            // Medium-lived objects hung off old ones, arrays, a map and statics.
            for (int i = 0; i < 4000; i++) {
                Node n = new Node(round * 100_000L + i);
                n.next = old[r.nextInt(old.length)].next;
                old[r.nextInt(old.length)].next = n;
                old[r.nextInt(old.length)].slots[r.nextInt(3)] = n;
                if ((i & 7) == 0) staticArray[r.nextInt(staticArray.length)] = n;
                if ((i & 15) == 0) map.put(r.nextInt(5000), n);
                recentStatic = n;
            }
            // Some of them die: links cut again.
            for (int i = 0; i < 3000; i++) {
                Node o = old[r.nextInt(old.length)];
                if (o.next != null && (r.nextInt(3) == 0)) o.next = o.next.next;
                o.slots[r.nextInt(3)] = null;
            }
            long s = 0;
            for (Node o : old) s = s * 7 + sum(o, 50);
            for (Object o : staticArray) if (o instanceof Node n) s += n.value;
            for (Node n : map.values()) s += n.value;
            s += recentStatic.value;
            total = total * 13 + s;
            if (round % 10 == 9) System.out.println("round " + round + " " + s);
        }
        System.out.println("total " + total);
    }
}
