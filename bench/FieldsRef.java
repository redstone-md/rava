// Reference-field hot path — getfield/putfield on OBJECT (reference) fields.
// MC object graphs are reference-field-heavy; the JIT scans the id array per
// access just like primitive fields. Measures native_getfield_ref/putfield_ref
// + the field-hint there.
public class FieldsRef {
    static final class Node {
        Node a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p;
        int v;
        Node(int v) { this.v = v; }
    }

    static int hot(int iters, Node q) {
        int acc = 0;
        for (int x = 0; x < iters; x++) {
            // read late ref fields (deep scans), write a ref field back
            Node t = q.p;
            q.h = q.o;
            acc += t.v + q.h.v + q.d.v;
            q.o = t;
        }
        return acc;
    }

    static Node make() {
        Node q = new Node(1);
        q.a = new Node(2); q.b = new Node(3); q.c = new Node(4); q.d = new Node(5);
        q.e = new Node(6); q.f = new Node(7); q.g = new Node(8); q.h = new Node(9);
        q.i = new Node(10); q.j = new Node(11); q.k = new Node(12); q.l = new Node(13);
        q.m = new Node(14); q.n = new Node(15); q.o = new Node(16); q.p = new Node(17);
        return q;
    }

    public static void main(String[] args) {
        for (int w = 0; w < 15; w++) hot(1_000_000, make()); // warmup → compile
        Node q = make();
        long t0 = System.nanoTime();
        int s = hot(20_000_000, q);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
