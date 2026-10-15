// Static fields read again in one method or loop: reused while nothing in between may store them
// (a call, a store of the same static, a class initialization, reflection), read again after.
public class StaticNumSem {
    static float r = 0.5f, g = 0.25f;
    static int k = 3;
    static volatile int vol;
    static int calls;

    static void change() { calls++; r += 0.125f; }

    static final class Late {
        static int seen;
        static { seen = k; k = 100; }
    }

    static float loop(int n) {
        float s = 0;
        for (int i = 0; i < n; i++) {
            s += r * 255 + g;
            if ((i & 1023) == 1023) change();
            if (i == n / 2) g = 0.75f;
        }
        return s;
    }

    static int init(boolean late) {
        int a = k;
        int b = late ? Late.seen : 0;
        return a * 1000 + b * 10 + k;
    }

    public static void main(String[] args) throws Exception {
        double s = 0;
        for (int i = 0; i < 300; i++) s += loop(5000);
        long t = 0;
        for (int i = 0; i < 100000; i++) t += init(i == 99990);
        java.lang.reflect.Field f = StaticNumSem.class.getDeclaredField("g");
        float u = 0;
        for (int i = 0; i < 100000; i++) {
            u += g;
            if (i == 50000) f.setFloat(null, 2f);
            u += g;
            vol = i;
        }
        System.out.println(s + " " + t + " " + u + " " + calls + " " + r + " " + k);
    }
}
