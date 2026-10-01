/** Field loads reused within a block (and invalidated by stores, calls and aliasing), and
 *  masked byte and short array loads. */
public class OptMemSem {
    int a, b;
    volatile int v;
    boolean flag;
    OptMemSem other;
    static int calls;

    static void touch(OptMemSem o) { o.a++; calls++; }

    static int loads(OptMemSem x, OptMemSem y) {
        int s = x.a + x.a;           // reused
        y.a = 7;                     // may alias x: x.a reloads
        s += x.a;
        x.b = 3;
        s += x.b * 10;               // forwarded
        touch(x);                    // call: reload
        s += x.a;
        s += x.v + x.v;              // volatile: never reused
        x.flag = (s & 2) != 0;
        s += x.flag ? 100 : 0;
        return s;
    }

    static int masks(byte[] bs, short[] ss, int i) {
        int x = bs[i] & 0xff, y = bs[i], z = ss[i] & 0xffff, w = ss[i], u = bs[i] & 0x7f;
        return x * 3 + y * 5 + z * 7 + w * 11 + u;
    }

    public static void main(String[] args) {
        OptMemSem p = new OptMemSem(), q = new OptMemSem();
        byte[] bs = {-1, -128, 127, 0, 1, (byte) 200};
        short[] ss = {-1, Short.MIN_VALUE, Short.MAX_VALUE, 0, 1, (short) 50000};
        long h = 0;
        for (int i = 0; i < 200000; i++) {
            h = h * 31 + loads(p, (i & 1) == 0 ? p : q);
            h = h * 31 + masks(bs, ss, i % bs.length);
        }
        System.out.println(h + " " + p.a + " " + q.a + " " + calls);
    }
}
