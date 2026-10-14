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
    static native void foreignThreads();
    static native char[] stringChars(String s);
    static native byte[] stringBytes(String s);
    static native String stringRoundTrip(String s);
    static native char[] stringRegion(String s, int start, int len);
    static native byte[] stringUtfRegion(String s, int start, int len);
    static native long stringGetterLoop(String s, int iterations, int kind);

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
    static final java.util.List<Thread> foreign = new java.util.ArrayList<>();

    /** Called by threads C started and attached (mode 0 named, 1 daemon, 2 no arguments, 3 a daemon never detached). */
    static void onForeign(int mode, boolean detachedBefore, boolean sameEnv) {
        Thread t = Thread.currentThread();
        String name = t.getName().startsWith("Thread-") ? "Thread-<n>" : t.getName();
        System.out.println("foreign " + mode + " detachedBefore=" + detachedBefore + " sameEnv=" + sameEnv + " name=" + name
                + " daemon=" + t.isDaemon() + " group=" + t.getThreadGroup().getName() + " alive=" + t.isAlive() + " state=" + t.getState());
        if (mode < 3) foreign.add(t);
    }

    static void onCall(int n, String s) { calls += n; System.out.println("callback " + n + " " + s); }

    /** DataOutputStream supplies an independent modified UTF-8 encoding oracle. */
    static byte[] modifiedUtf8(String s) throws java.io.IOException {
        java.io.ByteArrayOutputStream bytes = new java.io.ByteArrayOutputStream();
        new java.io.DataOutputStream(bytes).writeUTF(s);
        byte[] framed = bytes.toByteArray();
        return java.util.Arrays.copyOfRange(framed, 2, framed.length);
    }

    static void checkString(String s) throws java.io.IOException {
        if (!java.util.Arrays.equals(stringChars(s), s.toCharArray())) throw new AssertionError("UTF-16 chars");
        if (!java.util.Arrays.equals(stringBytes(s), modifiedUtf8(s))) throw new AssertionError("modified UTF-8 chars/length");
        if (!s.equals(stringRoundTrip(s))) throw new AssertionError("modified UTF-8 round trip");
        int[][] regions = {{0, s.length()}, {0, 0}, {s.length(), 0}, {s.length() / 2, s.length() - s.length() / 2}};
        for (int[] r : regions) {
            String sub = s.substring(r[0], r[0] + r[1]);
            if (!java.util.Arrays.equals(stringRegion(s, r[0], r[1]), sub.toCharArray())) throw new AssertionError("UTF-16 region");
            if (!java.util.Arrays.equals(stringUtfRegion(s, r[0], r[1]), modifiedUtf8(sub))) throw new AssertionError("modified UTF-8 region");
        }
    }

    static void checkStrings() {
        try {
            for (String s : new String[] {"", "ASCII", "\u0000\u007f\u0080\u00ff", "\u07ff\u0800\uffff",
                                           "\ud800x\udfff", "\ud83d\ude00"}) checkString(s);
            char[] large = new char[16384];
            for (int i = 0; i < large.length; i++) large[i] = (char) (i * 47);
            checkString(new String(large));
            int[][] invalid = {{-1, 1}, {0, -1}, {4, 0}, {Integer.MAX_VALUE, 1}, {0, Integer.MAX_VALUE}};
            for (int[] r : invalid) {
                try { stringRegion("abc", r[0], r[1]); throw new AssertionError("UTF-16 region bounds"); }
                catch (StringIndexOutOfBoundsException expected) {}
                try { stringUtfRegion("abc", r[0], r[1]); throw new AssertionError("modified UTF-8 region bounds"); }
                catch (StringIndexOutOfBoundsException expected) {}
            }
        } catch (java.io.IOException e) { throw new AssertionError(e); }
        System.out.println("strings: pass");
    }

    public static void main(String[] args) {
        System.loadLibrary("jnitest");
        // Optional microbenchmark: kind (0 length, 1 UTF length, 2 chars, 3 UTF chars),
        // code units, iterations, and coder (0 ASCII, 1 UTF-16). Each sample is a new process.
        if (args.length > 0 && args[0].equals("string-bench")) {
            int kind = Integer.parseInt(args[1]), n = Integer.parseInt(args[2]), iterations = Integer.parseInt(args[3]);
            boolean utf16 = args.length > 4 && args[4].equals("1");
            char[] chars = new char[n];
            for (int i = 0; i < n; i++) chars[i] = (char) (utf16 ? 0x800 + i % 1024 : 32 + i % 95);
            String s = new String(chars);
            stringGetterLoop(s, 2000, kind);
            long started = System.nanoTime();
            long checksum = stringGetterLoop(s, iterations, kind);
            System.out.println("checksum " + checksum + " ns/call " + (System.nanoTime() - started) / (double) iterations);
            return;
        }
        checkStrings();
        if (args.length > 0 && args[0].equals("strings")) return;
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
        foreignThreads();
        for (Thread f : foreign) System.out.println("after detach alive=" + f.isAlive() + " state=" + f.getState());
        System.gc();
        System.out.println("gc after a thread ended attached");
        twoLoaders();
    }

    // A second class loader's copy of a class binds its natives to its own copy of the library
    // (LWJGL bundled twice: each copy installs its own allocator in its own library).
    static void twoLoaders() {
        try {
            java.io.File dir = new java.io.File(".").getAbsoluteFile();
            java.io.File copy = new java.io.File(dir, "copy");
            copy.mkdirs();
            String lib = System.mapLibraryName("jnitest");
            java.nio.file.Files.copy(new java.io.File(dir, lib).toPath(), new java.io.File(copy, lib).toPath(), java.nio.file.StandardCopyOption.REPLACE_EXISTING);
            Copy.set(1);
            ClassLoader other = new java.net.URLClassLoader(new java.net.URL[] {dir.toURI().toURL()}, null);
            Class<?> c = Class.forName("JniTest$Copy", true, other);
            c.getMethod("load", String.class).invoke(null, new java.io.File(copy, lib).getPath());
            c.getMethod("set", int.class).invoke(null, 2);
            System.out.println("two loaders: own " + Copy.get() + " other " + c.getMethod("get").invoke(null) + " distinct " + (c != Copy.class));
        } catch (Exception e) {
            System.out.println("two loaders: " + e);
        }
    }

    public static class Copy {
        public static native void set(int v);

        public static native int get();

        public static void load(String path) {
            System.load(path);
        }
    }
}
