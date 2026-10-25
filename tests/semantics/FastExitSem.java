/** Early returns that optimized code takes before its prologue: base cases of recursions on
 *  int, long and reference tests, through a return block other paths share, with a constant,
 *  an argument or a little arithmetic as the result, and void. Each method recurses so that it
 *  is not inlined into its caller all the way down. */
public class FastExitSem {
    static int sum;

    static int down(int n, int a, int b) {
        if (n <= 0) return a ^ (b + 7);
        return down(n - 1, a + n, b - 1) + down(n - 2, b, a) & 0xffff;
    }

    static long longs(long n, long k) {
        if (n == 0L) return k - 3;
        return longs(n - 1, k * 3 + 1) + 1;
    }

    static int shared(int n) {
        int r;
        if (n < 3) r = n;
        else r = shared(n - 1) + shared(n - 3);
        return r;
    }

    static int merged(int n, int bias) {
        int r;
        if (n <= 0) r = (bias + 1) + 2;
        else r = merged(n - 1, bias * 3) + n;
        return r;
    }

    static long mergedConstant(int n) {
        long r;
        if (n <= 0) r = 17L;
        else r = mergedConstant(n - 1) + n;
        return r;
    }

    static int multiplyInt(int n, int x, int y) {
        if (n <= 0) return x * y;
        return multiplyInt(n - 1, x + 1, y - 3);
    }

    static int multiplyIntConstant(int n, int x) {
        if (n <= 0) return x * 31;
        return multiplyIntConstant(n - 1, -x);
    }

    static long multiplyLong(int n, long x, long y) {
        if (n <= 0) return x * y;
        return multiplyLong(n - 1, x + 1, y - 3);
    }

    static long multiplyLongConstant(int n, long x) {
        if (n <= 0) return x * 31L;
        return multiplyLongConstant(n - 1, -x);
    }

    static long multiplyLongWideConstant(int n, long x) {
        if (n <= 0) return x * 0x1_0000_0001L;
        return multiplyLongWideConstant(n - 1, x + 1);
    }

    static Object mergedRef(Object o, int n) {
        Object r;
        if (n <= 0) r = o;
        else r = mergedRef(o, n - 1);
        return r;
    }

    static int constant(int n) {
        if (n > 40) return -1;
        return constant(n + 1) + 2;
    }

    static int refs(Object o, int depth) {
        if (o == null) return depth;
        return refs(depth > 3 ? null : o, depth + 1) * 2;
    }

    static void effect(int n) {
        if (n == 0) return;
        sum += n;
        effect(n - 1);
    }

    static int negative(int n, int bias) {
        if (n != 0) return negative(n - 1, bias - 2) - 1;
        return bias | 1;
    }

    int cached;
    String name;
    FastExitSem next;

    int get(int n) {
        if (cached != 0) return cached ^ n;
        cached = n + 1;
        return get(n * 2) + 1;
    }

    String find(int depth) {
        if (name != null) return name;
        if (next == null) return "none";
        return next.find(depth + 1);
    }

    long weight(long w) {
        if (next == null) return w + cached;
        return next.weight(w + 1) + 1;
    }

    public static void main(String[] args) {
        FastExitSem a = new FastExitSem(), b = new FastExitSem(), c = new FastExitSem();
        a.next = b;
        b.next = c;
        long objs = 0;
        for (int i = 0; i < 4000; i++) {
            FastExitSem o = i % 3 == 0 ? new FastExitSem() : a;
            objs += o.get(i);
            c.name = i % 2 == 0 ? "c" + (i % 5) : null;
            objs += a.find(0).length();
            objs += a.weight(i);
        }
        System.out.println("objects " + objs + " " + a.find(0) + " " + a.weight(1));
        long check = 0;
        for (int i = 0; i < 4000; i++) {
            check += down(i % 9, i, -i);
            check += longs(i % 7, i * 1000000007L);
            check += shared(i % 12);
            check += merged(i % 9 - 2, i * 1000000007);
            check += mergedConstant(i % 9 - 2);
            check += mergedRef(a, i % 7) == a ? 1 : 0;
            check += mergedRef(null, i % 7) == null ? 1 : 0;
            check += constant(i % 45);
            check += refs(i % 5 == 0 ? null : "x", i % 4);
            effect(i % 10);
            check += negative(i % 6, i - 2000);
            check += multiplyInt(i % 3, i * 31 - 17, i * 101 + 1);
            check += multiplyIntConstant(i % 3, i * 997 - 2000);
            check += multiplyLong(i % 3, i * 12345678901L - 17, i * 0x1_0000_0001L + 1);
            check += multiplyLongConstant(i % 3, i * 0x1_0000_0001L - 2000);
            check += multiplyLongWideConstant(i % 3, i * 31L - 17);
        }
        System.out.println("check " + check + " sum " + sum);
        System.out.println("merged " + merged(0, Integer.MAX_VALUE) + " " + merged(-1, Integer.MIN_VALUE) + " " + merged(7, -3) + " " + mergedConstant(7));
        System.out.println(down(7, 5, 9) + " " + longs(5, Long.MAX_VALUE) + " " + shared(15) + " " + constant(0) + " " + refs("y", 0) + " " + negative(4, -9));
        System.out.println("int mul " + multiplyInt(0, 123, 456) + " " + multiplyInt(0, -123, 456) + " " + multiplyInt(0, Integer.MAX_VALUE, 17) + " " + multiplyInt(0, Integer.MIN_VALUE, -1) + " " + multiplyInt(1, Integer.MAX_VALUE, 17));
        System.out.println("int mul const " + multiplyIntConstant(0, 12345) + " " + multiplyIntConstant(0, -12345) + " " + multiplyIntConstant(0, Integer.MAX_VALUE) + " " + multiplyIntConstant(1, Integer.MIN_VALUE));
        System.out.println("long mul " + multiplyLong(0, 123456789L, 987654321L) + " " + multiplyLong(0, -123456789L, 987654321L) + " " + multiplyLong(0, Long.MAX_VALUE, 17) + " " + multiplyLong(0, Long.MIN_VALUE, -1) + " " + multiplyLong(1, Long.MAX_VALUE, 17));
        System.out.println("long mul const " + multiplyLongConstant(0, 1234567890123L) + " " + multiplyLongConstant(0, -1234567890123L) + " " + multiplyLongConstant(0, Long.MAX_VALUE) + " " + multiplyLongConstant(1, Long.MIN_VALUE));
        System.out.println("long mul wide const " + multiplyLongWideConstant(0, 7) + " " + multiplyLongWideConstant(0, Long.MAX_VALUE));
    }
}
