// A cast, instance test or identity hash whose object a dominating test proved non-null skips
// its own null test; one the object reaches as null on some path keeps it.
public class NonNullTestSem {
    interface Shape { int sides(); }
    static final class Sq implements Shape { public int sides() { return 4; } }
    static final class Tri implements Shape { public int sides() { return 3; } }
    static class Box { Object o; Box(Object o) { this.o = o; } }

    static int afterDeref(Box b) {
        Object o = b.o;
        int h = o.getClass() == Sq.class ? 1 : 0; // null check of o
        return h + ((Shape) o).sides() + (o instanceof Sq ? 10 : 20) + (System.identityHashCode(o) != 0 ? 100 : 0);
    }

    static int maybeNull(Object o) {
        Shape s = (Shape) o;                      // null passes the cast
        int r = o instanceof Shape ? 1 : 2;       // and fails the test
        return r + System.identityHashCode(o) * 0 + (s == null ? 5 : s.sides());
    }

    static int inLoop(Object[] os) {
        int sum = 0;
        for (Object o : os) {
            if (o != null && o.getClass() != Object.class) sum += ((Shape) o).sides();
            sum += (o instanceof Tri) ? 7 : 1;
            sum += System.identityHashCode(o) == 0 ? 2 : 3;
        }
        return sum;
    }

    public static void main(String[] args) {
        Object[] os = { new Sq(), null, new Tri(), null, new Sq() };
        long acc = 0;
        for (int r = 0; r < 40000; r++) {
            acc += afterDeref(new Box(r % 2 == 0 ? new Sq() : new Tri()));
            acc += maybeNull(r % 3 == 0 ? null : new Tri());
            acc += inLoop(os);
        }
        System.out.println("acc " + acc);
        try {
            afterDeref(new Box(null));
        } catch (NullPointerException e) {
            System.out.println("npe");
        }
        System.out.println(maybeNull(null) + " " + System.identityHashCode(null));
    }
}
