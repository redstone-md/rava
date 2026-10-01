// Differential probe: the language features a modern javac emits for this jar —
// sealed hierarchies, pattern matching in switch, records in patterns, text blocks.
// These compile to bytecode mechanisms (typeSwitch bootstraps, record deconstruction)
// that either exist in an engine or do not.
import java.util.*;
public class ModernSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }

    sealed interface Shape permits Circle, Square, Rect {}
    record Circle(double r) implements Shape {}
    record Square(double side) implements Shape {}
    record Rect(double w, double h) implements Shape {}

    sealed interface Expr permits Num, Add, Neg {}
    record Num(int v) implements Expr {}
    record Add(Expr l, Expr r) implements Expr {}
    record Neg(Expr e) implements Expr {}

    public static void main(String[] a) {
        // --- switch over a sealed hierarchy with type patterns
        List<Shape> shapes = List.of(new Circle(2), new Square(3), new Rect(2, 5));
        StringBuilder b = new StringBuilder();
        for (Shape s : shapes) b.append(area(s)).append(' ');
        p("area", b.toString().trim());

        // --- record deconstruction patterns
        p("deconstruct", describe(new Circle(1)) + " " + describe(new Rect(2, 2)) + " " + describe(new Square(4)));

        // --- guarded patterns
        p("guard", classify(new Rect(2, 2)) + " " + classify(new Rect(2, 5)) + " " + classify(new Circle(0)));

        // --- nested deconstruction and recursion over a sealed tree
        Expr e = new Add(new Num(2), new Neg(new Add(new Num(3), new Num(4))));
        p("eval", eval(e));
        p("show", show(e));

        // --- pattern matching for instanceof, with a guard
        Object o = "hello";
        p("instanceof", (o instanceof String s2 && s2.length() > 3) + " " + (o instanceof Integer i2 ? i2 : -1));

        // --- switch on enum with arrow form and yield
        p("enumSwitch", size(Sz.S) + " " + size(Sz.M) + " " + size(Sz.L));

        // --- null handling in a pattern switch
        p("nullCase", nullable(null) + " " + nullable("x") + " " + nullable(5));

        // --- text blocks
        String tb = """
            line1
              line2
            """;
        p("textBlock", tb.replace("\n", "|") + " len=" + tb.length());
        String joined = """
            a\
            b""";
        p("textBlock.join", joined);

        // --- var, enhanced for, and generics at runtime
        var list = new ArrayList<String>();
        list.add("v");
        p("var", list.get(0) + " " + list.getClass().getSimpleName());

        // --- switch with yield blocks
        p("yield", yielded(1) + " " + yielded(2) + " " + yielded(9));
    }
    enum Sz { S, M, L }
    static String area(Shape s) {
        return switch (s) {
            case Circle c -> "c" + (int) (c.r() * c.r());
            case Square q -> "q" + (int) (q.side() * q.side());
            case Rect r -> "r" + (int) (r.w() * r.h());
        };
    }
    static String describe(Shape s) {
        return switch (s) {
            case Circle(double r) -> "circle:" + (int) r;
            case Rect(double w, double h) -> "rect:" + (int) w + "x" + (int) h;
            case Square q -> "square:" + (int) q.side();
        };
    }
    static String classify(Shape s) {
        return switch (s) {
            case Rect(double w, double h) when w == h -> "square-ish";
            case Rect r -> "oblong";
            case Circle c when c.r() == 0 -> "point";
            default -> "other";
        };
    }
    static int eval(Expr e) {
        return switch (e) {
            case Num(int v) -> v;
            case Add(Expr l, Expr r) -> eval(l) + eval(r);
            case Neg(Expr x) -> -eval(x);
        };
    }
    static String show(Expr e) {
        return switch (e) {
            case Num n -> String.valueOf(n.v());
            case Add(Expr l, Expr r) -> "(" + show(l) + "+" + show(r) + ")";
            case Neg(Expr x) -> "-" + show(x);
        };
    }
    static int size(Sz s) {
        return switch (s) { case S -> 1; case M -> 2; case L -> 3; };
    }
    static String nullable(Object o) {
        return switch (o) {
            case null -> "null!";
            case String s -> "str:" + s;
            case Integer i -> "int:" + i;
            default -> "other";
        };
    }
    static int yielded(int n) {
        return switch (n) {
            case 1, 2 -> { int t = n * 10; yield t + 1; }
            default -> { yield -1; }
        };
    }
}
