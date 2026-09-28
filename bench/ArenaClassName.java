// Regression probe for getClass()/toString() on ARENA-BORN objects. Nothing here is
// Minecraft-specific: a plain user class is arena-born under the default engine, and
// before the class_name_of fix both methods reported java.lang.Object for it.
public class ArenaClassName {
    static class Node { final Object a, b; Node(Object a, Object b) { this.a = a; this.b = b; } }
    record Pair(Object x, Object y) {}
    public static void main(String[] args) {
        Node n = new Node("a", "b");
        Pair p = new Pair(1, 2);
        Object[] arr = new Object[2];
        System.out.println("node.getClass=" + n.getClass().getName());
        System.out.println("pair.getClass=" + p.getClass().getName());
        System.out.println("arr.getClass=" + arr.getClass().getName());
        System.out.println("str.getClass=" + "s".getClass().getName());
        // Object.toString() embeds the class name, so it fails the same way.
        String ts = new Object() {}.getClass().getName();
        System.out.println("anon.nonempty=" + (!ts.equals("java.lang.Object")));
        System.out.println("node.toString.prefix=" + n.toString().split("@")[0]);
    }
}
