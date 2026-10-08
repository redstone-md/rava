// A field declared with a final class holds exactly that class or null: calls on its value
// bind statically (here `toString`/`hashCode`, whose profiles other receivers pollute), and a
// null in it still throws.
public class DeclaredTypeSem {
    static final class Name { final String ns, path; Name(String a, String b) { ns = a; path = b; } }
    static final class Holder { String s; Integer i; Holder(String s, Integer i) { this.s = s; this.i = i; } }
    static String label = "lbl";

    static int use(Holder h) {
        Object o = h.s;
        Object p = h.i;
        return o.toString().length() + o.hashCode() % 97 + p.hashCode() + label.toString().length();
    }

    static String key(Name n) { return n.ns + "." + n.path; }

    public static void main(String[] args) {
        // Pollute the shared call sites with other receivers first.
        Object[] mixed = { 1, 2L, 'c', Boolean.TRUE, java.util.List.of(1), "s" };
        int junk = 0;
        for (int r = 0; r < 20000; r++) junk += mixed[r % mixed.length].toString().length() + mixed[r % mixed.length].hashCode() % 7;
        long acc = 0;
        Holder h = new Holder("value", 41);
        Name n = new Name("minecraft", "overworld");
        for (int r = 0; r < 30000; r++) acc += use(h) + key(n).length();
        System.out.println("acc " + acc + " " + junk + " " + key(n));
        try {
            use(new Holder(null, 1));
        } catch (NullPointerException e) {
            System.out.println("npe s");
        }
        try {
            use(new Holder("x", null));
        } catch (NullPointerException e) {
            System.out.println("npe i");
        }
        label = null;
        try {
            use(h);
        } catch (NullPointerException e) {
            System.out.println("npe static");
        }
        System.out.println(key(new Name(null, "p")));
    }
}
