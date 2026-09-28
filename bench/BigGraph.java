// Pointer-chasing over a graph far larger than L2 — the probe for GC COPY-ORDER
// locality (RAVA_GC_DFS_COPY) and, together with RAVA_JIT_BARE_REF_LOAD, for what a
// reference dereference costs once the value model's tag discrimination is gone.
//
// Why not FieldsRef: its graph is 17 nodes (~1 KB) and lives in L1, so object
// PLACEMENT cannot matter there — it measures the tag ALU, not the cache. Here the
// live set is tens of MB and the traversal follows child pointers, so every hop is a
// candidate miss and placement is exactly what is being measured.
//
// The build interleaves two trees so a breadth-first copying collector cannot get
// parent/child adjacency for free, and it drops every other subtree to force real
// evacuation work. Traversal order is data-dependent (derived from the accumulator)
// so the prefetcher cannot linearise it, but it is fully deterministic: SUM is a gate.
public class BigGraph {
    static final class Node {
        Node a, b;
        int v;
        Node(int v) { this.v = v; }
    }

    // FULL binary tree — 2^depth - 1 nodes, so depth alone sets the working-set size
    // (depth 18 ≈ 262k nodes, far past L2). Garbage comes from building several trees
    // and keeping only the last, which forces the collector to evacuate a live graph
    // out of a nursery full of dead one — the situation whose copy ORDER is at issue.
    static Node build(int depth, int seed) {
        if (depth == 0) return null;
        Node n = new Node(seed);
        n.a = build(depth - 1, seed * 2 + 1);
        n.b = build(depth - 1, seed * 2 + 2);
        return n;
    }

    static int count(Node n) {
        return n == null ? 0 : 1 + count(n.a) + count(n.b);
    }

    // FULL-TREE traversal, so the working set per pass IS the whole graph. A
    // root-to-leaf descent would touch only ~depth nodes and stay in L1 — the flaw
    // that makes FieldsRef blind to placement. Recursion follows child pointers, so
    // the access order is exactly the pointer graph, not a linear sweep.
    static int sum(Node n) {
        if (n == null) return 0;
        return n.v + sum(n.a) + sum(n.b);
    }

    static int walk(Node root, int iters) {
        int acc = 0;
        for (int i = 0; i < iters; i++) {
            acc += sum(root);
            acc ^= (acc << 3);
        }
        return acc;
    }

    public static void main(String[] args) {
        int depth = args.length > 0 ? Integer.parseInt(args[0]) : 20;
        int iters = args.length > 1 ? Integer.parseInt(args[1]) : 400_000;
        Node root = null;
        // Several build rounds so the graph survives collections and gets copied.
        for (int r = 0; r < 4; r++) root = build(depth, r + 1);
        System.out.println("NODES=" + count(root)); // the probe proves its own working set
        for (int w = 0; w < 3; w++) walk(root, iters / 10); // warm the JIT
        long t0 = System.nanoTime();
        int s = walk(root, iters);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
