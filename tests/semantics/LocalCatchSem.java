// An exception thrown and caught in the same method, which optimized code compiles as a jump to
// the handler: catch by class and by superclass, a class the handler does not catch (caught
// further out), a null exception (NullPointerException instead), finally blocks, a handler
// inside a loop, a throw inside a synchronized block caught outside it, and locals the handler
// reads as they were at the throw.
public class LocalCatchSem {
    static final RuntimeException SHARED = new IllegalStateException("shared");
    static final Object LOCK = new Object();

    static int pick(int i) {
        int x = i * 3;
        try {
            if ((i & 3) == 1) throw SHARED;
            if ((i & 3) == 2) throw new IllegalArgumentException("arg" + i);
            if ((i & 7) == 3) throw new UnsupportedOperationException();
            if ((i & 15) == 7) throw (RuntimeException) null;
            x += 1;
        } catch (IllegalStateException e) {
            return x + 100 + e.getMessage().length();
        } catch (IllegalArgumentException e) {
            return x + 200 + e.getMessage().length();
        } catch (NullPointerException e) {
            return x + 300;
        } catch (RuntimeException e) {
            return x + 400;
        } finally {
            x = -1;
        }
        return x;
    }

    static int loop(int n) {
        int caught = 0, sum = 0;
        for (int i = 0; i < n; i++) {
            try {
                if (i % 5 == 0) throw SHARED;
                sum += i;
            } catch (RuntimeException e) {
                caught++;
                sum -= caught;
            }
        }
        return sum * 31 + caught;
    }

    static int locked(int i) {
        int r = 0;
        try {
            synchronized (LOCK) {
                r = i;
                if (i % 3 == 0) throw SHARED;
                r += 10;
            }
        } catch (IllegalStateException e) {
            r = -r;
        }
        return r + (Thread.holdsLock(LOCK) ? 1000000 : 0);
    }

    static int outer(int i) {
        try {
            try {
                if (i % 2 == 0) throw new java.io.UncheckedIOException(new java.io.IOException("io"));
                return 1;
            } catch (IllegalStateException e) {
                return 2;
            }
        } catch (java.io.UncheckedIOException e) {
            return 3;
        }
    }

    public static void main(String[] args) {
        long s = 0;
        for (int i = 0; i < 200_000; i++) s += pick(i) + locked(i) + outer(i);
        System.out.println(s);
        System.out.println(loop(100_000));
    }
}
