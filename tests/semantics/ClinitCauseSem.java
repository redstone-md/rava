// A class whose static initializer failed: the first use throws ExceptionInInitializerError,
// every later one NoClassDefFoundError("Could not initialize class ..."), whose cause (since
// JDK 18) names the original exception, its message and thread, with its stack trace.
public class ClinitCauseSem {
    static class Broken {
        static final int VALUE = compute();

        static int compute() {
            throw new IllegalStateException("config missing");
        }
    }

    static class BrokenNoMessage {
        static int value = Integer.parseInt(null);
    }

    static class Child extends Broken {
        static int extra = 1;
    }

    static void use(String what, Runnable r) {
        try {
            r.run();
            System.out.println(what + ": ok");
        } catch (Throwable e) {
            System.out.println(what + ": " + e);
            for (Throwable c = e.getCause(); c != null; c = c.getCause()) {
                System.out.println("  caused by " + c);
                StackTraceElement[] st = c.getStackTrace();
                System.out.println("  top frame " + (st.length > 0 ? st[0].getClassName() + "." + st[0].getMethodName() : "none"));
            }
        }
    }

    public static void main(String[] args) throws Exception {
        use("first", () -> System.out.println(Broken.VALUE));
        use("second", () -> System.out.println(Broken.VALUE));
        use("child", () -> System.out.println(Child.extra));
        use("no message", () -> System.out.println(BrokenNoMessage.value));
        use("no message again", () -> System.out.println(BrokenNoMessage.value));
        Thread other = new Thread(() -> use("other thread", () -> System.out.println(Broken.VALUE)), "worker-7");
        other.start();
        other.join();
    }
}
