// Does Objects.equals dispatch to the guest's own equals? Every generated equals and every
// JDK-shaped helper delegates to it, so answering identity alone degrades a value type's equality
// to reference equality everywhere it is reached through this method.
import java.util.Objects;

public class ObjEq {
    static final class P {
        final int x, y;
        P(int x, int y) { this.x = x; this.y = y; }
        @Override public boolean equals(Object o) {
            return o instanceof P && ((P) o).x == x && ((P) o).y == y;
        }
        @Override public int hashCode() { return x * 31 + y; }
        @Override public String toString() { return "P(" + x + "," + y + ")"; }
    }
    record R(int a, String b) {}

    public static void main(String[] args) {
        P p1 = new P(1, 2), p2 = new P(1, 2), p3 = new P(3, 4);
        System.out.println("eq_same=" + Objects.equals(p1, p2));
        System.out.println("eq_diff=" + Objects.equals(p1, p3));
        System.out.println("eq_null_l=" + Objects.equals(null, p1));
        System.out.println("eq_null_r=" + Objects.equals(p1, null));
        System.out.println("eq_null_both=" + Objects.equals(null, null));
        System.out.println("eq_str=" + Objects.equals(new String("ab"), "ab"));
        R r1 = new R(7, "k"), r2 = new R(7, "k");
        System.out.println("eq_rec=" + Objects.equals(r1, r2));
        System.out.println("hash_eq=" + (Objects.hashCode(p1) == Objects.hashCode(p2)));
        System.out.println("hash_str=" + (Objects.hashCode("ab") == "ab".hashCode()));
        System.out.println("hash_null=" + Objects.hashCode(null));
        System.out.println("hashes_eq=" + (Objects.hash(1, "a") == Objects.hash(1, "a")));
        System.out.println("toString=" + Objects.toString(p1));
    }
}
