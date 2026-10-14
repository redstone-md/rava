// Exceptions an inlined callee throws and its caller catches, which optimized code compiles as
// a jump from the callee's body to the caller's handler: exceptions of a caught class, of
// another class (caught further out), null, from a synchronized callee (its monitor released),
// from a callee's callee, from a callee that catches some itself, and the caller's locals and
// stack as they were at the call.
public class InlinedCatchSem {
    static final IllegalStateException STATE = new IllegalStateException("state");
    static int counter;

    static int check(int i) {
        if ((i & 3) == 1) throw STATE;
        if ((i & 7) == 2) throw new IllegalArgumentException("arg " + i);
        if ((i & 15) == 6) throw (RuntimeException) null;
        counter++;
        return i * 2;
    }

    static synchronized int locked(int i) {
        if (i % 3 == 0) throw STATE;
        return i;
    }

    static int deeper(int i) {
        return check(i) + 1;
    }

    static int selfCatching(int i) {
        try {
            if (i % 4 == 0) throw STATE;
        } catch (IllegalStateException e) {
            return -1;
        }
        if (i % 4 == 1) throw new UnsupportedOperationException();
        return 5;
    }

    static int caller(int i) {
        int before = i + 7;
        long wide = i * 3L;
        try {
            return before + check(i);
        } catch (IllegalStateException e) {
            return before * 2 + (int) wide;
        } catch (NullPointerException e) {
            return -before;
        }
    }

    static int outer(int i) {
        try {
            return caller(i);
        } catch (IllegalArgumentException e) {
            return 1000 + e.getMessage().length();
        }
    }

    static int sync(int i) {
        try {
            return locked(i);
        } catch (IllegalStateException e) {
            return -2 + (Thread.holdsLock(InlinedCatchSem.class) ? 1000000 : 0);
        }
    }

    static int twoLevels(int i) {
        try {
            return deeper(i);
        } catch (RuntimeException e) {
            return e == STATE ? 77 : 88;
        }
    }

    static int mixed(int i) {
        try {
            return selfCatching(i);
        } catch (UnsupportedOperationException e) {
            return 9;
        }
    }

    public static void main(String[] args) {
        long s = 0;
        for (int i = 0; i < 300_000; i++) s += outer(i) + sync(i) + twoLevels(i) + mixed(i);
        System.out.println(s + " " + counter);
    }
}
