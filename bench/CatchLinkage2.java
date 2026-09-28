// getstatic-triggered <clinit> failure must be catchable at the triggering
// frame (netty's Slf4JLoggerFactory NOP check is exactly this shape).
public class CatchLinkage2 {
    static class T2 {
        static int X = 7;
        static {
            if (true) throw new NoClassDefFoundError("gs-boom");
        }
    }

    static int trigger() {
        return T2.X; // getstatic → clinit → throw
    }

    public static void main(String[] a) {
        try {
            System.out.println("V=" + trigger());
        } catch (LinkageError e) {
            System.out.println("CAUGHT-GS=" + e.getClass().getName() + ":" + e.getMessage());
        }
        System.out.println("OK");
    }
}
