package ravatest.app;

/** The program the test patch edits: each line of `run` shows one kind of edit. */
public class PatchApp {
    public interface Shape {
        int area();
    }

    static final class Square implements Shape {
        private final int side;

        Square(int side) {
            this.side = side;
        }

        public int area() {
            return side * side;
        }
    }

    public static int twice(int x) {
        return 2 * x;
    }

    public String greet(String who) {
        return "hello " + who;
    }

    void run() {
        System.out.println(greet("world"));
        System.out.println(twice(21));
        Shape s = new Square(3);
        System.out.println(s.area());
        Box box = new Box(7);
        System.out.println(Peek.peek(box));
        System.out.println(box.value());
    }

    public static void main(String[] args) {
        PatchApp app = new PatchApp();
        app.run();
        // Not redirected: only calls inside `run` are.
        System.out.println(app.greet("again") + " " + twice(1));
    }
}

final class Box {
    private final int v;

    Box(int v) {
        this.v = v;
    }

    int value() {
        return v;
    }
}

final class Peek {
    static String peek(Object o) {
        return "unpatched";
    }
}
