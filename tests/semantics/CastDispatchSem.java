import java.util.*;

// Casts sunk below the class tests that prove them (opt/castdispatch.rs): a raw list's
// element of another class still fails its cast at the cast, a third subclass takes the
// fallback, a null element still throws at the call.
public class CastDispatchSem {
    interface Shape { double area(); }
    static final class Sq implements Shape { final double s; Sq(double s) { this.s = s; } public double area() { return s * s; } }
    static final class Ci implements Shape { final double r; Ci(double r) { this.r = r; } public double area() { return 3 * r * r; } }
    static final class Tri implements Shape { final double b; Tri(double b) { this.b = b; } public double area() { return b / 2; } }

    static int seen;

    static double sum(List<Shape> list) {
        double s = 0;
        seen = 0;
        for (Shape sh : list) {
            s += sh.area();
            seen++;
        }
        return s;
    }

    @SuppressWarnings({"unchecked", "rawtypes"})
    public static void main(String[] args) {
        List<Shape> list = new ArrayList<>();
        for (int i = 0; i < 64; i++) list.add((i & 1) == 0 ? new Sq(i) : new Ci(i));
        double t = 0;
        for (int r = 0; r < 30000; r++) t += sum(list);
        System.out.println("warm " + t);
        List raw = new ArrayList<>(list);
        raw.add(17, "not a shape");
        try {
            sum(raw);
        } catch (ClassCastException e) {
            System.out.println("cce after " + seen);
        }
        List<Shape> third = new ArrayList<>(list);
        third.add(40, new Tri(9));
        System.out.println("third " + sum(third) + " " + seen);
        List<Shape> holes = new ArrayList<>(list);
        holes.set(23, null);
        try {
            sum(holes);
        } catch (NullPointerException e) {
            System.out.println("npe after " + seen);
        }
        for (int r = 0; r < 30000; r++) t += sum(third);
        System.out.println("again " + t + " " + sum(list));
    }
}
