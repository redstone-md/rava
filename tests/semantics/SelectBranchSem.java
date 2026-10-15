// Boolean tests inlined from two receivers (one a range test of the character, the other a
// table lookup) merge into a value the caller branches on, and choices made of other choices.
public class SelectBranchSem {
    interface Pred { boolean is(int c); }
    static final class Digit implements Pred { public boolean is(int c) { return ((c - '0') | ('9' - c)) >= 0; } }
    static final class Table implements Pred {
        final boolean[] t = new boolean[128];
        Table(String s) { for (char c : s.toCharArray()) t[c] = true; }
        public boolean is(int c) { return c < 128 && t[c]; }
    }

    static int span(Pred p, String s, int i) {
        int n = 0;
        while (i < s.length() && p.is(s.charAt(i))) { i++; n++; }
        return n;
    }

    static int twice(int c) {
        boolean a = c > 10;
        boolean b = !a;
        return (b ? 3 : 7) + (a == b ? 100 : 0);
    }

    public static void main(String[] args) {
        Pred[] ps = {new Digit(), new Table("abc%-+ "), new Digit()};
        String[] ss = {"12345abc", "abc  12", "%-+9", "", "999999999x"};
        long h = 0;
        for (int r = 0; r < 200000; r++)
            for (int j = 0; j < ss.length; j++) h = h * 31 + span(ps[(r + j) % 3], ss[j], r & 1) + twice(r % 23);
        System.out.println(h);
    }
}
