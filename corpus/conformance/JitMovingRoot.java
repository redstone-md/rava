// Keep one arena object only in a compiled activation local while allocation forces
// repeated moving collections. The final read must observe the forwarded reference.
public class JitMovingRoot {
    static final class Node {
        int value;
        Node(int value) { this.value = value; }
    }

    static int exercise() {
        Node retained = new Node(1337);
        int sum = 0;
        for (int i = 0; i < 200_000; i++) {
            Node garbage = new Node(i);
            sum += garbage.value;
        }
        return retained.value ^ sum;
    }

    public static void main(String[] args) {
        System.out.println("RESULT=" + exercise());
    }
}
