// Counted loops of many strips (the optimizing tier polls once per 1024 iterations): limits
// near Integer.MAX_VALUE and below the start, early exits, a trap inside a strip, nested
// loops, values merged after the loop, and a long loop without calls while another thread
// allocates (its collections need this thread at a safepoint).
public class StripMineSem {
    static abstract class Op { abstract long apply(long s, int x); }
    static final class Add extends Op { long apply(long s, int x) { return s + x; } }
    static final class Mix extends Op { long apply(long s, int x) { return s * 31 + (x ^ 5); } }
    static final Op[] OPS = {new Add(), new Mix(), new Add()};
    static int[] marks = new int[64];

    // Bodies of several blocks (a two-class call, a conditional store): they are strip-mined
    // rather than unrolled.
    static long sum(int[] a, int from, int to) {
        long s = 0;
        for (int i = from; i < to; i++) {
            int x = a[i & (a.length - 1)] + i;
            s = OPS[(i & 0x7fffffff) % 3].apply(s, x);
            if (x < 0) marks[i & 63]++;
        }
        return s;
    }

    static int firstOver(int[] a, int n, int bound) {
        int found = -1;
        for (int i = 0; i < n; i++) {
            if (a[i] > bound) { found = i; break; }
        }
        return found;
    }

    static long nearMax(int from, int limit) {
        long s = 0;
        for (int i = from; i < limit; i++) {
            s = OPS[i & 1].apply(s, i & 7);
            if ((i & 15) == 3) marks[i & 63]--;
        }
        return s;
    }

    static long trap(int[] a, int n) {
        long s = 0;
        try {
            for (int i = 0; i < n; i++) {
                s = OPS[i % 3].apply(s, a[i]);
                if (s < 0) marks[i & 63]++;
            }
        } catch (ArrayIndexOutOfBoundsException e) {
            s = -s;
        }
        return s;
    }

    static long nested(int n, int m) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            for (int j = 0; j < m; j++) {
                s = OPS[j & 1].apply(s, i * j);
                if ((j & 255) == 0) marks[j & 63]++;
            }
        }
        return s;
    }

    static int merged(int[] a, int n) {
        int i = 0, last = 0;
        for (; i < n; i++) {
            last = (int) OPS[i % 3].apply(last, a[i % a.length]);
            if (last == 7) marks[0]++;
        }
        return i * 7 + last;
    }

    static volatile boolean stop;

    static long spin(int[] a, int n) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            s = OPS[i & 1].apply(s, a[i & 1023]);
            if (s == 12345) marks[1]++;
        }
        return s;
    }

    public static void main(String[] args) throws Exception {
        int[] a = new int[1 << 12];
        for (int i = 0; i < a.length; i++) a[i] = (i * 2654435761L) % 1000 > 500 ? i : -i;
        long acc = 0;
        for (int r = 0; r < 300; r++) {
            acc += sum(a, r, 5000 + r) + sum(a, 10, 3) + sum(a, -r, r) + sum(a, 0, 1024) + sum(a, 0, 1025);
            acc += firstOver(a, a.length, 4000 - r) + firstOver(a, a.length, 1 << 20);
            acc += nearMax(Integer.MAX_VALUE - 3000 - r, Integer.MAX_VALUE) + nearMax(Integer.MAX_VALUE, Integer.MAX_VALUE)
                + nearMax(Integer.MAX_VALUE - 1, Integer.MAX_VALUE) + nearMax(Integer.MIN_VALUE, Integer.MIN_VALUE + 2100 + r);
            acc += trap(a, a.length + (r & 1)) + trap(a, 2048);
            acc += nested(40 + (r & 3), 1500) + merged(a, 3000 + r) + merged(a, 0);
        }
        for (int m : marks) acc = acc * 7 + m;
        System.out.println(acc);
        Thread alloc = new Thread(() -> {
            long junk = 0;
            while (!stop) {
                Object[] o = new Object[1000];
                junk += o.length;
            }
            System.out.println(junk > 0);
        });
        alloc.start();
        long s = 0;
        for (int r = 0; r < 10; r++) s += spin(a, 20_000_000);
        stop = true;
        alloc.join();
        System.out.println(s);
    }
}
