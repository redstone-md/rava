/** JNI end to end (`tests/jni/run.sh`): natives of every argument and return kind, strings,
 *  arrays, fields, callbacks into Java, exceptions, RegisterNatives and global references. */
public class JniTest {
    int count = 7;
    static String label = "static";

    static native int add(int a, int b);
    static native long mix(byte b, short s, char c, int i, long l, float f, double d, boolean z);
    static native double many(double a, int b, double c, int d, double e, int f, double g, int h,
                              double i, int j, double k, int l, double m, int n, double o, int p, double q, double r);
    static native String greet(String name);
    static native int sum(int[] values);
    static native void fill(byte[] out, byte v);
    static native Object pass(Object o);
    native int bump(int by);
    static native void callBack(int n);
    static native void raise(String message);
    static native int registered(int x);
    static native String globalRoundTrip(String s);
    static native int[] makeArray(int n);

    static int calls;
    static volatile boolean done;

    /** One of each kind of call, for compiled callers. */
    static long hot(int i, JniTest t) {
        long r = add(i, 1) + mix((byte) i, (short) i, (char) i, i, i, i * 0.5f, i * 0.25, (i & 1) == 0)
                + (long) many(i, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, i)
                + greet("y" + (i & 7)).length() + sum(new int[] {i, 1}) + t.bump(1) + registered(i);
        Object o = new int[i & 7];
        if (pass(o) != o) r += 1_000_000;
        if ((i & 4095) == 0) {
            try {
                raise("hot");
            } catch (IllegalStateException e) {
                r += 7;
            }
        }
        return r;
    }
    static void onCall(int n, String s) { calls += n; System.out.println("callback " + n + " " + s); }

    public static void main(String[] args) {
        System.loadLibrary("jnitest");
        System.out.println(add(40, 2));
        System.out.println(mix((byte) -3, (short) -300, 'x', 100000, 1L << 40, 1.5f, 2.25, true));
        System.out.println(many(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18));
        System.out.println(greet("rava"));
        System.out.println(sum(new int[] {1, 2, 3, 4}));
        byte[] b = new byte[4];
        fill(b, (byte) 9);
        System.out.println(java.util.Arrays.toString(b));
        Object o = new Object();
        System.out.println(pass(o) == o);
        System.out.println(pass(null));
        JniTest t = new JniTest();
        System.out.println(t.bump(5) + " " + t.count);
        callBack(3);
        System.out.println("calls " + calls);
        try {
            raise("from C");
        } catch (IllegalStateException e) {
            System.out.println("caught " + e.getMessage());
        }
        System.out.println(registered(20));
        System.out.println(globalRoundTrip("kept"));
        System.out.println(java.util.Arrays.toString(makeArray(5)));
        // Many calls: local references must not pile up, objects must survive collections.
        long total = 0;
        for (int i = 0; i < 200000; i++) {
            total += greet("x" + (i & 15)).length();
            if ((i & 1023) == 0) System.gc();
        }
        System.out.println("total " + total);
        // The same natives from compiled code, with another thread allocating meanwhile so that
        // collections stop this one around its native calls.
        Thread churn = new Thread(() -> {
            Object[] keep = new Object[64];
            for (int i = 0; !done; i++) keep[i & 63] = new byte[16384 + (i & 255)];
        });
        churn.start();
        long acc = 0;
        for (int i = 0; i < 300000; i++) acc += hot(i, t);
        done = true;
        try {
            churn.join();
        } catch (InterruptedException e) {
            throw new RuntimeException(e);
        }
        System.out.println("hot " + acc + " " + t.count);
    }
}
