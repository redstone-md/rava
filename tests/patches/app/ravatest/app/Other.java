package ravatest.app;

/** Edited in each loader that defines it: `local` becomes the patch's shared counter. */
public final class Other {
    public static int run() {
        return local();
    }

    static int local() {
        return 0;
    }
}
