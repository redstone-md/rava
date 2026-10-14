// invokestatic initializes the method's class even when the method does nothing: LWJGL's
// bindings call an empty LibX.initialize() from their own static initializer so that LibX's
// (which installs the library's allocator) runs first. Interpreted, compiled and inlined.
public class EmptyStaticInitSem {
    static StringBuilder log = new StringBuilder();

    static class Lib {
        static {
            log.append("Lib.<clinit> ");
        }

        static void initialize() {}
    }

    static class Binding {
        static {
            Lib.initialize();
            log.append("Binding.<clinit> ");
        }

        static int use(int x) { return x + 1; }
    }

    static class Lib2 {
        static {
            log.append("Lib2.<clinit> ");
        }

        static void initialize() {}
    }

    static void warm(boolean last) {
        if (last) Lib2.initialize();
    }

    public static void main(String[] args) {
        System.out.println(Binding.use(1));
        System.out.println(log);
        // A hot caller that reaches the empty method only on its last call (compiled by then).
        for (int i = 0; i < 300_000; i++) warm(i == 299_999);
        System.out.println(log);
    }
}
