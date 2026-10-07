// Type checks after type tests, hot enough to run as optimized code: a cast after an
// instanceof of the same class or a subclass, instanceof after a cast (null passes a cast),
// interfaces, pattern matching, a cast to an unrelated class still throwing, and a null
// check of a value a test proved non-null.
import java.util.ArrayList;
import java.util.List;

public class TypeCheckSem {
    interface Shape { int area(); }
    static class Sq implements Shape { final int s; Sq(int s) { this.s = s; } public int area() { return s * s; } }
    static final class Cube extends Sq { Cube(int s) { super(s); } public int area() { return 6 * s * s; } }
    static final class Circle implements Shape { final int r; Circle(int r) { this.r = r; } public int area() { return 3 * r * r; } }

    static int classify(Object o) {
        int s = 0;
        if (o instanceof Sq) {
            Sq q = (Sq) o;
            s += q.s;
            Shape sh = (Shape) o;
            s += sh.area();
            if (o instanceof Shape) s += 1;
            if (o instanceof Cube c) s += c.s * 100;
        }
        if (o instanceof Shape sh && !(o instanceof Sq)) s += sh.area() * 7;
        return s;
    }

    static int afterCast(Object o) {
        Object x = (Shape) o;
        // A cast lets null through: this instanceof must still see it.
        return (x instanceof Shape ? 1 : 0) + (x instanceof Circle ? 10 : 0);
    }

    static int wrongCast(Object o) {
        if (o instanceof Sq) {
            try {
                Circle c = (Circle) o;
                return c.r;
            } catch (ClassCastException e) {
                return -1;
            }
        }
        return 0;
    }

    static int fields(Object o) {
        if (o instanceof Sq q) {
            Sq again = (Sq) o;
            return q.s + again.s + (again == q ? 1 : 0);
        }
        return o == null ? -5 : -7;
    }

    public static void main(String[] args) {
        List<Object> xs = new ArrayList<>();
        for (int i = 0; i < 64; i++) {
            xs.add(switch (i % 6) {
                case 0 -> new Sq(i);
                case 1 -> new Cube(i % 5);
                case 2 -> new Circle(i % 7);
                case 3 -> "str" + i;
                case 4 -> null;
                default -> Integer.valueOf(i);
            });
        }
        long a = 0, b = 0, c = 0, d = 0;
        for (int r = 0; r < 3000; r++) {
            for (Object o : xs) {
                a += classify(o);
                if (!(o instanceof String) && !(o instanceof Integer)) b += afterCast(o);
                c += wrongCast(o);
                d += fields(o);
            }
        }
        System.out.println(a + " " + b + " " + c + " " + d);
    }
}
