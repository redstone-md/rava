// Chains of reference loads (a.b.c.d, a.arr.length), which optimized code addresses through the
// compressed reference before decoding it: chains of every length, null in the middle (an
// implicit null check that throws), values reused after the chain, across collections.
public class PtrChainSem {
    static final class Node { Node next; int[] data; String name; int v; Node(int v) { this.v = v; } }

    static int chain(Node a) { return a.next.next.next.v + a.next.data.length + a.next.next.name.length(); }

    static int reuse(Node a) {
        Node b = a.next;
        int x = b.next.v;
        return x + b.v + b.data[0] + a.v;
    }

    static int guarded(Node a) {
        try {
            return a.next.next.next.next.v;
        } catch (NullPointerException e) {
            return -1;
        }
    }

    public static void main(String[] args) {
        Node[] heads = new Node[64];
        for (int i = 0; i < heads.length; i++) {
            Node n = null;
            for (int k = 0; k < 6; k++) {
                Node m = new Node(i * 10 + k);
                m.next = n;
                m.data = new int[k + 1];
                m.data[0] = k;
                m.name = "n" + k;
                n = m;
            }
            heads[i] = n;
        }
        // Some chains end early: the fourth hop meets null.
        for (int i = 0; i < heads.length; i += 7) heads[i].next.next.next.next = null;
        long s = 0;
        for (int r = 0; r < 4000; r++) {
            for (Node h : heads) s += chain(h) + reuse(h) + guarded(h);
            if (r % 500 == 0) {
                Object[] junk = new Object[10000];
                for (int j = 0; j < junk.length; j++) junk[j] = new Node(j);
                System.gc();
            }
        }
        System.out.println(s);
    }
}
