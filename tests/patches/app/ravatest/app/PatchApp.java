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
        System.out.println("shared " + Other.run());
    }

    public static void main(String[] args) {
        PatchApp app = new PatchApp();
        app.run();
        // Not redirected: only calls inside `run` are.
        System.out.println(app.greet("again") + " " + twice(1));
        // A second copy of Other, in a loader of its own, gets the same patch classes.
        java.net.URL dir = PatchApp.class.getProtectionDomain().getCodeSource().getLocation();
        try (java.net.URLClassLoader l = new java.net.URLClassLoader(new java.net.URL[] {dir}, ClassLoader.getPlatformClassLoader())) {
            Class<?> other = l.loadClass("ravatest.app.Other");
            System.out.println("shared " + other.getMethod("run").invoke(null) + " " + (other != Other.class));
        } catch (Exception e) {
            System.out.println(e);
        }
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
