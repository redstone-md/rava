// Differential probe: exception semantics — messages, causes, suppression, finally
// ordering, try-with-resources. The text of an exception is guest-visible output and
// this jar logs it, so a wrong message is a wrong answer.
// THREE UNDECLARED DIVERGENCES, kept RED (see bench\CollSem.java for the other three):
//
//   stack.nonempty  `new Throwable().getStackTrace()` returns an EMPTY array. The frames
//   stack.method    exist - the interpreter has a call stack - they are simply not
//                   captured into the Throwable, so every logged stack trace this jar
//                   prints is blank where it should name a method.
//
//   npe.field       this engine's NPE message is `java/lang/String.length`; the JVM emits
//                   a helpful message naming the expression. Guest-visible text, and this
//                   jar logs it.
import java.util.*;
public class ExcSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    static String ex(Runnable r) {
        try { r.run(); return "NO THROW"; }
        catch (Throwable t) { return t.getClass().getName() + ": " + t.getMessage(); }
    }
    static class Res implements AutoCloseable {
        final String n; final boolean bad;
        Res(String n, boolean bad) { this.n = n; this.bad = bad; System.out.println("open " + n); }
        public void close() { System.out.println("close " + n); if (bad) throw new IllegalStateException("close " + n); }
    }
    public static void main(String[] a) {
        // --- the standard messages
        p("npe.field", ex(() -> { String s = null; s.length(); }));
        p("aioobe", ex(() -> { int[] x = new int[2]; int y = x[5]; }));
        p("aioobe.neg", ex(() -> { int[] x = new int[2]; int y = x[-1]; }));
        p("negarray", ex(() -> { int[] x = new int[-1]; }));
        p("cce", ex(() -> { Object o = "s"; Integer i = (Integer) o; }));
        p("ase", ex(() -> { Object[] x = new String[1]; x[0] = 1; }));
        p("arith", ex(() -> { int z = 1 / 0; }));
        p("arith.mod", ex(() -> { int z = 1 % 0; }));
        p("nfe", ex(() -> Integer.parseInt("x")));
        p("oob.string", ex(() -> "ab".substring(5)));
        p("oob.list", ex(() -> new ArrayList<>(List.of(1)).get(3)));
        p("nosuch", ex(() -> new ArrayList<Integer>().iterator().next()));
        p("emptyDeque", ex(() -> new ArrayDeque<Integer>().pop()));
        p("emptyOptional", ex(() -> Optional.empty().get()));
        p("uoe", ex(() -> List.of(1).add(2)));

        // --- cause chains and suppression
        Exception root = new IllegalArgumentException("root");
        Exception wrap = new RuntimeException("wrap", root);
        p("cause", wrap.getCause().getMessage() + " " + (wrap.getCause() == root));
        p("cause.toString", wrap.toString());
        p("initCause", ex(() -> { Exception e = new Exception("a"); e.initCause(root); e.initCause(root); }));
        Exception sup = new RuntimeException("main");
        sup.addSuppressed(new IllegalStateException("s1"));
        sup.addSuppressed(new IllegalStateException("s2"));
        p("suppressed", sup.getSuppressed().length + " " + sup.getSuppressed()[0].getMessage());
        p("noMessage", new RuntimeException().getMessage() + " " + new RuntimeException().toString());

        // --- finally ordering and return interaction
        p("finally.order", finallyOrder());
        p("finally.overrides", finallyOverrides());
        p("nested", nested());

        // --- try-with-resources: close order, and suppression when both throw
        try (Res r1 = new Res("A", false); Res r2 = new Res("B", false)) { System.out.println("body"); }
        try {
            try (Res r1 = new Res("C", true)) { throw new RuntimeException("body-boom"); }
        } catch (Throwable t) {
            p("twr.suppressed", t.getMessage() + " suppressed=" + t.getSuppressed().length
                + " " + (t.getSuppressed().length > 0 ? t.getSuppressed()[0].getMessage() : "-"));
        }

        // --- catch ordering and rethrow
        p("multicatch", multicatch(0) + " " + multicatch(1) + " " + multicatch(2));
        p("instanceof", (root instanceof RuntimeException) + " " + (wrap instanceof RuntimeException)
            + " " + (root instanceof Exception));

        // --- stack trace shape (depth only; frames are engine-specific)
        StackTraceElement[] st = new Exception().getStackTrace();
        p("stack.nonempty", st.length > 0);
        p("stack.method", st.length > 0 ? st[0].getMethodName() : "-");
    }
    @SuppressWarnings("finally")
    static String finallyOrder() {
        StringBuilder b = new StringBuilder();
        try { b.append("t"); throw new RuntimeException("x"); }
        catch (RuntimeException e) { b.append("c"); }
        finally { b.append("f"); }
        return b.toString();
    }
    @SuppressWarnings("finally")
    static int finallyOverrides() {
        try { return 1; } finally { return 2; }
    }
    static String nested() {
        StringBuilder b = new StringBuilder();
        try {
            try { throw new IllegalStateException("inner"); }
            finally { b.append("if"); }
        } catch (IllegalStateException e) { b.append("oc").append(e.getMessage()); }
        return b.toString();
    }
    static String multicatch(int mode) {
        try {
            if (mode == 0) throw new IllegalStateException("i");
            if (mode == 1) throw new NumberFormatException("n");
            throw new RuntimeException("r");
        } catch (IllegalStateException | NumberFormatException e) { return "specific:" + e.getMessage(); }
        catch (RuntimeException e) { return "general:" + e.getMessage(); }
    }
}
