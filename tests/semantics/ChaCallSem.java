/**
 * A method no loaded class overrides, called but not inlined (recursive): compiled code calls
 * it directly; loading an overriding class later, also while a compiled caller's loop runs,
 * must send calls with its instances to the override.
 */
public class ChaCallSem {
    static class Node {
        int weight() { return 1; }
        int depth(int n) { return n == 0 ? weight() : depth(n - 1) + n % 3; }
    }

    // Loaded only when first used, after the callers are hot.
    static class Heavy extends Node {
        @Override int depth(int n) { return n == 0 ? 100 : depth(n - 1) + 7; }
    }

    static Node[] nodes = new Node[16];

    static long walk(int rounds, int switchAt) {
        long total = 0;
        for (int r = 0; r < rounds; r++) {
            if (r == switchAt) nodes[r % 16] = make("ChaCallSem$Heavy");
            for (Node x : nodes) total += x.depth(12);
        }
        return total;
    }

    static Node make(String name) {
        try {
            return (Node) Class.forName(name).getDeclaredConstructor().newInstance();
        } catch (ReflectiveOperationException e) {
            throw new RuntimeException(e);
        }
    }

    public static void main(String[] args) {
        for (int i = 0; i < nodes.length; i++) nodes[i] = new Node();
        long warm = 0;
        for (int i = 0; i < 3000; i++) warm += walk(20, -1);
        System.out.println("warm " + warm);
        // The override arrives while the compiled loop runs.
        System.out.println("mid-loop " + walk(100000, 50000));
        long again = 0;
        for (int i = 0; i < 3000; i++) again += walk(20, -1);
        System.out.println("after " + again);
    }
}
