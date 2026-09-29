package dyn;

public class BadInit {
    static int VALUE = fail();

    static int fail() {
        throw new IllegalStateException("clinit-boom");
    }
}
