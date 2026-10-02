/**
 * A non-escaping object that stays live across a speculative call: when the call's receiver
 * guard fails after the method got hot, the optimized code deoptimizes while the object is
 * live, so the interpreter it hands off to must see the object's fields. This exercises
 * scalar replacement with rematerialization on the deopt path.
 */
public class OptScalarSem {
    interface Op { int apply(int x); }
    static final class Inc implements Op { public int apply(int x) { return x + 1; } }
    static final class Dbl implements Op { public int apply(int x) { return x * 2; } }

    static final class Box {
        final int a, b;
        Box(int a, int b) { this.a = a; this.b = b; }
    }

    // box never escapes and is live across op.apply; its fields are read after the call.
    static long hot(Op op, int x) {
        Box box = new Box(x, x * 10 + 3);
        int via = op.apply(box.a);
        return (long) via + box.b - box.a;
    }

    // A capturing lambda, non-escaping, live across a guarded call: the spline shape.
    static long hotLambda(Op op, int x) {
        java.util.function.IntUnaryOperator add = v -> v + x;
        int via = op.apply(x);
        return (long) via + add.applyAsInt(x);
    }

    public static void main(String[] args) {
        Op inc = new Inc(), dbl = new Dbl();
        long sum = 0;
        for (int i = 0; i < 40000; i++) {
            // Monomorphic Inc while the methods compile, then Dbl forces the guard to fail.
            Op op = i < 20000 ? inc : dbl;
            sum += hot(op, i & 255);
            sum += hotLambda(op, (i * 7) & 255);
        }
        System.out.println("sum " + sum);
    }
}
