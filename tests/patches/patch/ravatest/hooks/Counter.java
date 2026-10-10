package ravatest.hooks;

/** A second patch class, loaded on first use from the edited code's loader. */
final class Counter {
    private static int n;

    static int next() {
        return ++n;
    }
}
