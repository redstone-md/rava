// The bounds contracts a native re-implementation is free to get silently wrong.
//
// Every case here has a JVM answer that is an EXCEPTION where a clamping
// implementation returns a plausible shorter string, an unchanged buffer or a
// null — differences no gate hash can see, because they are text a guest reads.
// Run on both engines and diff line by line.
public class BoundsEx {
    static void p(String tag, Runnable r) {
        try { r.run(); System.out.println(tag + " NO-THROW"); }
        catch (Throwable t) { System.out.println(tag + " " + t.getClass().getName() + ": " + t.getMessage()); }
    }

    public static void main(String[] a) {
        char[] cs = {'a', 'b', 'c'};
        byte[] bs = {65, 66, 67};
        int[] is = {97, 98, 99};

        p("newStr-C-ok", () -> { new String(cs, 1, 2); });
        p("newStr-C-over", () -> { new String(cs, 1, 5); });
        p("newStr-C-neg", () -> { new String(cs, -1, 2); });
        p("newStr-C-null", () -> { new String((char[]) null, 0, 1); });
        p("newStr-B-over", () -> { new String(bs, 2, 4); });
        p("newStr-B-negcount", () -> { new String(bs, 0, -1); });
        p("newStr-I-over", () -> { new String(is, 0, 9); });

        p("valueOf-ok", () -> { String.valueOf(cs, 1, 2); });
        p("valueOf-over", () -> { String.valueOf(cs, 1, 9); });
        p("valueOf-neg", () -> { String.valueOf(cs, -2, 1); });
        p("copyValueOf-over", () -> { String.copyValueOf(cs, 0, 7); });

        StringBuilder sb = new StringBuilder("abc");
        p("sb-charAt-ok", () -> { sb.charAt(2); });
        p("sb-charAt-over", () -> { sb.charAt(3); });
        p("sb-charAt-neg", () -> { sb.charAt(-1); });
        p("sb-del-over", () -> { new StringBuilder("abc").deleteCharAt(5); });
        p("sb-del-neg", () -> { new StringBuilder("abc").deleteCharAt(-1); });
        p("sb-set-over", () -> { new StringBuilder("abc").setCharAt(3, 'z'); });
        p("sb-sub-over", () -> { new StringBuilder("abc").substring(1, 9); });
        p("sb-sub-inverted", () -> { new StringBuilder("abc").substring(2, 1); });
        p("sb-sub-neg", () -> { new StringBuilder("abc").substring(-1, 2); });
        StringBuilder ok = new StringBuilder("abc");
        ok.setCharAt(1, 'Z');
        ok.deleteCharAt(0);
        System.out.println("sb-mutate " + ok + " sub=" + new StringBuilder("hello").substring(1, 3));

        p("replaceAll-ok", () -> { System.out.println("ra " + "a1b2".replaceAll("([a-z])(\\d)", "$2$1")); });
        p("replaceAll-nogroup", () -> { "a1b2".replaceAll("([a-z])", "$3"); });
        p("replaceAll-nomatch", () -> { System.out.println("rn " + "xyz".replaceAll("(q)", "$5")); });
        p("replaceAll-escaped", () -> { System.out.println("re " + "a1".replaceAll("(a)", "\\$1x")); });
        p("replaceFirst-nogroup", () -> { "a1b2".replaceFirst("(\\d)", "$7"); });

        java.util.List<String> l = new java.util.ArrayList<>(java.util.List.of("p", "q"));
        p("list-get-over", () -> { l.get(4); });
        p("list-get-neg", () -> { l.get(-1); });
        System.out.println("list-get-ok " + l.get(1));
        System.out.println("opt-get " + java.util.Optional.of("v").get()
            + " atomic=" + new java.util.concurrent.atomic.AtomicReference<>("w").get());
    }
}
