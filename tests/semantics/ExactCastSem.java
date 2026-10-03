// checkcast and instanceof of objects whose class optimized code knows exactly (fresh
// allocations, values cast to a final class, possibly null): casts to supertypes pass without
// a check, instanceof folds, and casts that must fail still throw ClassCastException.
import java.util.function.Supplier;

public class ExactCastSem {
    static final class Box implements Supplier<Integer> { final int v; Box(int v) { this.v = v; } public Integer get() { return v; } }
    static Object make(int i) { return i % 5 == 0 ? null : new Box(i); }
    static int hot(int i) {
        Object o = new Box(i);
        Supplier<?> s = (Supplier<?>) o;
        int r = (o instanceof Supplier ? 1 : 0) + (o instanceof Runnable ? 10 : 0) + (Integer) s.get();
        Object m = make(i);
        Box b = (Box) m;
        r += (b instanceof Supplier ? 100 : 0) + (m instanceof Box ? 1000 : 0);
        return r;
    }
    static String bad(Object o) {
        try { return String.valueOf((Runnable) o); } catch (ClassCastException e) { return "CCE"; }
    }
    public static void main(String[] args) {
        long acc = 0;
        for (int i = 0; i < 300000; i++) acc += hot(i);
        System.out.println(acc + " " + bad(new Box(1)) + " " + bad(null));
    }
}
