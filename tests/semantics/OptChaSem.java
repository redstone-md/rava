/**
 * Class hierarchy analysis: a method no loaded class overrides is inlined without a receiver
 * check; loading an overriding class later (even while the compiled caller runs a loop) must
 * send calls with its instances to the override.
 */
public class OptChaSem {
    static class Shape {
        int sides() { return 4; }
        int area(int s) { return s * s; }
    }

    static int sum(Shape[] shapes, int rounds) {
        int total = 0;
        for (int r = 0; r < rounds; r++) {
            for (Shape s : shapes) total += s.sides() + s.area(r & 7);
        }
        return total;
    }

    // Loaded only when first used, after sum() is hot.
    static class Triangle extends Shape {
        @Override int sides() { return 3; }
    }

    static class Circle extends Shape {
        @Override int area(int s) { return 3 * s * s; }
    }

    static Shape[] shapes = new Shape[64];

    static int loop(int rounds, int switchAt) {
        int total = 0;
        for (int r = 0; r < rounds; r++) {
            if (r == switchAt) shapes[r % 64] = make("OptChaSem$Triangle");
            for (Shape s : shapes) total += s.sides();
        }
        return total;
    }

    static Shape make(String name) {
        try {
            return (Shape) Class.forName(name).getDeclaredConstructor().newInstance();
        } catch (ReflectiveOperationException e) {
            throw new RuntimeException(e);
        }
    }

    public static void main(String[] args) {
        for (int i = 0; i < shapes.length; i++) shapes[i] = new Shape();
        int warm = 0;
        for (int i = 0; i < 2000; i++) warm += sum(shapes, 20);
        System.out.println("warm " + warm);
        shapes[5] = make("OptChaSem$Circle");
        System.out.println("with circle " + sum(shapes, 20));
        // The override arrives while the compiled loop runs.
        System.out.println("mid-loop " + loop(200000, 100000));
        int again = 0;
        for (int i = 0; i < 2000; i++) again += sum(shapes, 20);
        System.out.println("after " + again);
    }
}
