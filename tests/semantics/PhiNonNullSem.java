// A phi whose every input is non-null on its edge (tested there, allocated, a chain walked only
// while non-null) needs no null test after the merge; one with a null input keeps it.
public class PhiNonNullSem {
    static final class Node { final int key; Node next; final String val; Node(int k, String v, Node n) { key = k; val = v; next = n; } }

    static String find(Node head, int k) {
        Node e = head;
        while (e != null && e.key != k) e = e.next;
        return e == null ? null : e.val;
    }

    static int merged(Node a, Node b, int k) {
        Node x;
        if (a != null) x = a;
        else if (k > 3) x = new Node(k, "new", null);
        else x = b;
        return x == null ? -1 : x.key;
    }

    static int chain(Node head) {
        int n = 0;
        Node e = head;
        if (e != null) {
            do { n += e.key; e = e.next; } while (e != null);
        }
        return n;
    }

    public static void main(String[] args) {
        Node list = null;
        for (int i = 0; i < 20; i++) list = new Node(i, "v" + i, list);
        long acc = 0;
        for (int r = 0; r < 60000; r++) {
            String f = find(list, r % 25);
            acc += f == null ? -3 : f.length();
            acc += merged((r & 1) == 0 ? list : null, (r & 2) == 0 ? list.next : null, r & 7);
            acc += chain((r & 3) == 0 ? null : list);
        }
        System.out.println("acc " + acc);
    }
}
