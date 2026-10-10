package ravatest.hooks;

/** One class for every loader resolving it, so its state is shared. */
public final class Shared {
    private static int n;

    private Shared() {}

    public static int next() {
        return ++n;
    }
}
