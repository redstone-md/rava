// String concatenation with literal pieces between values: one- to many-byte separators (their
// bytes are stored directly into the new array), a literal longer than sixteen bytes, Latin-1
// and UTF-16 values (the literal then inflates), null values, and numbers.
public class ConcatLitSem {
    static String a(String x, String y) { return x + "." + y; }
    static String b(String x, String y, int n) { return x + ", " + y + "::" + n + "|"; }
    static String c(String x) { return "[" + x + "] abcdefghijklmnopqrstuvwxyz " + x + "]"; }
    static String d(String x, char ch) { return x + "/é/" + ch + "!"; }

    public static void main(String[] args) {
        String[] xs = { "minecraft", "overworld", "", "été", "中文", null, "a", "longer_string_value_here" };
        long h = 0;
        StringBuilder first = new StringBuilder();
        for (int r = 0; r < 40000; r++) {
            for (int i = 0; i < xs.length; i++) {
                String x = xs[i], y = xs[(i + r) % xs.length];
                String s1 = a(x, y), s2 = b(x, y, r % 1000 - 500), s3 = c(y), s4 = d(x, (char) ('a' + r % 26));
                h = h * 31 + s1.hashCode() + 7L * s2.hashCode() + 13L * s3.hashCode() + 17L * s4.hashCode() + s1.length() + s4.length();
                if (r == 0) first.append(s1).append(s2).append(s3).append(s4).append('\n');
            }
        }
        System.out.print(first);
        System.out.println("h " + h);
    }
}
