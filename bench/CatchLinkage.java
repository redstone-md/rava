// Minimal repro: an Error thrown inside a <clinit> must propagate to the
// TRIGGERING call site and be catchable there as LinkageError (netty's
// InternalLoggerFactory.newDefaultFactory relies on exactly this to fall back
// from a failed Slf4JLoggerFactory to the next logging backend; if the catch
// is missed the error escapes to top and — in the boot's chunk path — is
// swallowed by the chunk system as "chunk absent" → silent regeneration).
public class CatchLinkage {
    static class Thrower {
        static {
            if (true) throw new NoClassDefFoundError("boom");
        }
        static void poke() {}
    }

    public static void main(String[] a) {
        try {
            Thrower.poke();
        } catch (LinkageError e) {
            System.out.println("CAUGHT=" + e.getClass().getName() + ":" + e.getMessage());
        }
        // Second touch: JVM reports NoClassDefFoundError for an already-failed class.
        try {
            Thrower.poke();
        } catch (LinkageError e) {
            System.out.println("CAUGHT2=" + e.getClass().getName());
        }
        System.out.println("OK");
    }
}
