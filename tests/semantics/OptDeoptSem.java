import java.util.ArrayList;
import java.util.List;
import java.util.function.IntUnaryOperator;

/**
 * Semantics of optimized code leaving through deoptimization: exceptions raised inside inlined
 * callees and caught by their callers, stack traces through inlined frames, guards failing after
 * the code got hot (null, bounds, casts, division, receiver classes), monitors and lambdas.
 */
public class OptDeoptSem {
    interface Shape { int area(); }
    static final class Sq implements Shape { final int s; Sq(int s) { this.s = s; } public int area() { return s * s; } }
    static final class Rect implements Shape { final int w, h; Rect(int w, int h) { this.w = w; this.h = h; } public int area() { return w * h; } }
    static class Base { int f(int x) { return x + 1; } }
    static class Derived extends Base { @Override int f(int x) { return x * 3; } }

    static int[] data = new int[64];
    static final Object lock = new Object();
    static int counter;

    static int leaf(int[] a, int i) { return a[i] * 2; }
    static int middle(int[] a, int i) { return leaf(a, i) + 1; }
    static int outer(int[] a, int i) {
        try {
            return middle(a, i);
        } catch (ArrayIndexOutOfBoundsException e) {
            return -1000 - i;
        }
    }

    static int divide(int a, int b) { return a / b; }
    static int safeDivide(int a, int b) {
        try { return divide(a, b); } catch (ArithmeticException e) { return Integer.MIN_VALUE; }
    }

    static int len(String s) { return s.length(); }
    static int nullable(String s) {
        try { return len(s); } catch (NullPointerException e) { return -1; }
    }

    static int areaOf(Object o) { return ((Shape) o).area(); }
    static int castOrMinus(Object o) {
        try { return areaOf(o); } catch (ClassCastException e) { return -7; }
    }

    static int dispatch(Base b, int x) { return b.f(x); }
    static int shapes(Shape s) { return s.area(); }

    static int locked(int n) {
        synchronized (lock) {
            counter += n;
            return counter;
        }
    }

    static int thrower(int depth) {
        if (depth == 0) throw new IllegalStateException("deep");
        return thrower(depth - 1) + 1;
    }

    static String trace(Throwable t) {
        StringBuilder sb = new StringBuilder(t.getClass().getName());
        for (StackTraceElement e : t.getStackTrace()) {
            if (!e.getClassName().startsWith("OptDeoptSem")) break;
            sb.append(" <- ").append(e.getMethodName());
        }
        return sb.toString();
    }

    static int viaLambda(IntUnaryOperator f, int x) { return f.applyAsInt(x); }

    public static void main(String[] args) {
        for (int i = 0; i < data.length; i++) data[i] = i;
        long sum = 0;
        for (int round = 0; round < 20000; round++) {
            int i = round % 70;
            sum += outer(data, i);
            sum += safeDivide(round, round % 13);
            sum += nullable(round % 17 == 0 ? null : "abc");
            sum += castOrMinus(round % 19 == 0 ? (Object) "x" : new Sq(round % 5));
            sum += dispatch(round < 15000 ? new Base() : new Derived(), round);
            sum += shapes(round % 3 == 0 ? new Rect(2, round % 7) : new Sq(round % 4));
            sum += locked(1);
            int k = round;
            sum += viaLambda(x -> x + k, 3);
        }
        System.out.println("sum " + sum + " counter " + counter);
        try {
            outer(null, 0);
        } catch (NullPointerException e) {
            System.out.println("npe " + trace(e));
        }
        for (int r = 0; r < 3; r++) {
            try {
                thrower(3);
            } catch (IllegalStateException e) {
                System.out.println("trace " + trace(e));
            }
        }
        List<Integer> list = new ArrayList<>();
        for (int i = 0; i < 50000; i++) list.add(i % 11);
        long t = 0;
        for (int i = 0; i < 20; i++) for (int v : list) t += v;
        System.out.println("list " + t);
    }
}
