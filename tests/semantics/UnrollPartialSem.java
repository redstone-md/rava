// Counted loops of unknown trip count, which optimized code runs four iterations per test with
// the rest in the original loop: every trip count around the factor, limits near
// Integer.MAX_VALUE and below zero, an exception thrown by one of the copies (the interpreter
// resumes at that iteration), and a store that a later iteration reads back.
public class UnrollPartialSem {
    static int sum(int[] a, int n) {
        int s = 0;
        for (int i = 0; i < n; i++) s += a[i] * (i + 1);
        return s;
    }

    static long fromTo(int from, int to) {
        long s = 0;
        for (int i = from; i < to; i++) s += i & 7;
        return s;
    }

    static int prefix(int[] a, int n) {
        for (int i = 1; i < n; i++) a[i] += a[i - 1];
        return a[n - 1];
    }

    static String overrun(int[] a, int n) {
        int s = 0;
        int i = 0;
        try {
            for (i = 0; i < n; i++) s += a[i];
            return "sum " + s;
        } catch (ArrayIndexOutOfBoundsException e) {
            return "stopped at " + i + " with " + s;
        }
    }

    public static void main(String[] args) {
        int[] a = new int[64];
        for (int i = 0; i < a.length; i++) a[i] = i * 3 - 7;
        StringBuilder out = new StringBuilder();
        for (int round = 0; round < 3000; round++) {
            out.setLength(0);
            for (int n = 0; n <= 13; n++) out.append(sum(a, n)).append(' ');
            out.append(fromTo(Integer.MAX_VALUE - 9, Integer.MAX_VALUE)).append(' ');
            out.append(fromTo(Integer.MIN_VALUE, Integer.MIN_VALUE + 6)).append(' ');
            out.append(fromTo(-20, -3)).append(' ');
            out.append(fromTo(5, -5)).append(' ');
            int[] p = new int[19];
            for (int i = 0; i < p.length; i++) p[i] = i;
            out.append(prefix(p, p.length)).append(' ');
            for (int n = 60; n <= 70; n++) out.append(overrun(a, n)).append(", ");
        }
        System.out.println(out);
    }
}
